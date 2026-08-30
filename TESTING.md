# NSMT — Testing Guide (测试指南)

> 记录本项目的测试架构、已知难点/卡点、以及辅助工具集，供后续开发与测试复用。
> 配合 `scripts/e2e.sh` 一键端到端测试使用。

## 1. 测试分层

| 层 | 方式 | 覆盖 | 命令 |
|---|---|---|---|
| 单元测试 | `#[cfg(test)]` in-crate | 帧编解码、哈希、密钥、锁、域池分片、E2E 轮换、ObjectStore | `cargo test` |
| 端到端 | `scripts/e2e.sh`（临时目录 + 真机进程） | 7 场景：租户/控制API/双机同步/P2P/冲突GUI/监督器重启/配额/备份恢复 | `./scripts/e2e.sh [--release] [--keep]` |
| 覆盖率 | cargo-tarpaulin | 统计各 crate 行覆盖 | `cargo tarpaulin --out lcov` |
| 快速重跑 | cargo-nextest | 并行加速 + 失败缓存 | `cargo nextest run` |
| 依赖安全 | cargo-audit | CVE 扫描 | `cargo audit` |

## 2. 已知难点与卡点 (Gotchas)

### 2.1 Rust 并行测试污染进程级环境变量 ⚠️（已修复）
- **现象**：`memory.rs` 的 `pool_from_env_*` 用例偶发失败——两个测试并行跑时互相 set/remove
  `NSMT_POOL_GATEWAYS`，断言 `shard_count` 不一致。
- **根因**：cargo test 默认多线程并行；环境变量是**进程级全局**，测试间共享，无隔离。
- **对策**：所有修改 env 的用例共用一个 `static ENV_LOCK: Mutex<()>`（测试模块内），
  串行化这些用例；不依赖 env 的用例不受影响。**新测试若动 env，必须加锁。**

### 2.2 端口冲突（ygg 与 ygg-admin 同端口）
- **现象**：E2E 第 5 步 ygg-admin spawn 的 ygg 起不来（`pid=null`）——之前直启的服务器仍占着 `16666`。
- **根因**：监督器测试要接管服务器生命周期，但旧进程未停。
- **对策**：脚本里启动 ygg-admin 前先 `kill` 直启服务器；**以后加"接管型"测试记住先释放资源。**

### 2.3 环境变量作用域泄漏（内联 env 只对单命令生效）
- **现象**：`conflicts-web` 测试扫不到冲突文件——脚本前面用 `NSMT_SHARE_DIR=xxx cmd` 内联赋值，
  仅对那条命令生效，后续 GUI 进程读的是默认 `~/nsmt_share`。
- **对策**：GUI 步骤前显式 `export NSMT_SHARE_DIR="$SB"`。**跨进程共享配置务必 export，别用内联。**
  （这也是为什么固定记忆 marker 需要 `~/.nsmt/share.path` 落盘——进程间传递路径靠文件而非 env。）

### 2.4 QUIC 客户端配置多 provider
- **现象**：`rustls` 同时拉 aws-lc-rs 与 ring 时需显式
  `rustls::crypto::aws_lc_rs::default_provider().install_default()`，否则 panic。
- **对策**：server/client 每个入口都先 install_default；新二进制（如 ygg-admin）若建 QUIC 连接也要照做。

### 2.5 进程清理与僵尸
- **现象**：早期监督器用 `std::mem::forget(child)` 分离进程 → 子进程退出变僵尸，
  `kill -0` 对僵尸仍返回存活 → 永不重启。
- **对策**：持有 `Child` 句柄 + `try_wait()` 轮询并回收（已修复，`wait_child`）。

### 2.6 时区/时间戳断言
- 目录树 `tree_hash` 含 `mtime_ns`，测试建文件后立即 build_tree 可能 mtime 相同 → diff 无差异。
- 对策：端到端场景断言"文件存在/内容一致"而非精确时间；改文件后 sleep 防抖（脚本里 200ms 防抖 + 轮询 5s）。

## 3. 工具集 (Toolchain)

| 工具 | 用途 | 安装 |
|---|---|---|
| `jq` | 解析控制 API / 测试断言 JSON | `brew install jq` |
| `watch` | 观察进程/端口/日志变化 | `brew install watch` |
| `tree` | 查看共享目录/对象缓存结构 | `brew install tree` |
| `lsof` / `tcpdump` / `nc` | 端口占用、UDP/QUIC 抓包、连通性 | 系统自带 |
| `cargo-nextest` | 快速并行测试运行器 | `cargo install cargo-nextest` |
| `cargo-tarpaulin` | 行覆盖率 | `cargo install cargo-tarpaulin` |
| `cargo-audit` | 依赖 CVE 审计 | `cargo install cargo-audit` |
| `gh` | 仓库/CI/Pages 管理 | `brew install gh` |
| `python3` | 临时 JSON/哈希/地址计算 | 系统自带 |

### 一键 E2E：`scripts/e2e.sh`
```bash
./scripts/e2e.sh            # debug 构建跑 7 场景
./scripts/e2e.sh --release  # release 构建
./scripts/e2e.sh --keep     # 保留测试目录（排查用）
```

## 4. 排查技巧

- **看日志**：`RUST_LOG=debug`；E2E 脚本把每个进程输出落到 `/tmp/nsmt-e2e.*/*.log`，
  失败时 `grep -E "pull|peer|auth|diff" <log>` 快速定位。
- **看端口**：`lsof -iUDP:16666` / `watch -n1 'lsof -iUDP:16666'`。
- **看共享树**：`python3 -c "import json;d=json.load(open('.../trees/latest.json'));print([e['path'] for e in d['entries']])"`
- **P2P 链路**：服务器 `object not found; peer=` 提示 + 客户端 `peer fetch OK` 日志 = 直连成功；
  只有 `peer auth rejected` = 对等认证失败（多半是域密钥不一致）。

## 5. 安全审计（cargo audit，2026-08-30）

```bash
cargo audit
```

| 漏洞 | 严重度 | 来源（传递依赖） | 影响 | 状态 |
|---|---|---|---|---|
| quick-xml RUSTSEC-2026-0194/0195 | high | `object_store 0.11` → S3 XML 解析 | 仅 S3 后端启用时 | 需升 object_store ≥0.14（有 breaking change，暂缓） |
| rsa RUSTSEC-2023-0071 | medium | `sqlx-mysql`（MySQL 认证） | 仅用 MySQL 驱动时 | 上游无修复版；SQLite/Postgres 不受影响 |

> 结论：默认部署（本地后端 + SQLite）不受影响；S3/MySQL 用户建议关注上游修复。
> 升级路径：`object_store 0.14`（改 `put/get/head` 调用）、`sqlx` 新版本（等 rsa 修复）。

## 6. 覆盖率（cargo tarpaulin，2026-08-30）

- 行覆盖率 **13.7%**（LF=2483 / LH=341）。
- 说明：核心链路主要靠 `scripts/e2e.sh` 端到端验证（真机进程级），单元覆盖偏低；
  **改进方向**：为 `nsmt-core`（帧/加密/身份）与 `nsmt-fs`（Prefixed store）补单测，
  这两部分无 IO 依赖、最容易把覆盖率拉上去。
