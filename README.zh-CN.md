# NSMT / Yggdrasil — Net Share Memory Tree（中文）

> 多租户、多机器、多 agent 的「共享记忆 + 共享文件」网络层。
> English version: [README.md](README.md)

NSMT（代号 **Yggdrasil**，北欧神话世界树）让不同机器上的 agent（Maka / Hermes / 自定义）在同一
**用户域**下共享一棵记忆树与一份共享文件 —— 支持身份鉴权、文件锁、断点续传、P2P 直连、端到端加密、
配额管理与 Web 管理后台。

---

## 特性

| 里程碑 | 特性 |
|---|---|
| M0–M2 | 身份（FQN/Ed25519）· QUIC 传输 · 在线注册表 · 记忆双写/托底 · 共享文件 CAS/目录树/锁 |
| M3–M4 | 鉴权加固 · 冲突处理 · 对象存储抽象 · 断点续传 · S3 后端 · P2P mesh 基础 · 冲突合并 CLI |
| M5 | MinIO S3 实机联调 · 多租户配额 · E2E 加密 · 交互式冲突合并 |
| M6 | 控制 API（`--control :8091`）· 用户系统（sqlx Any：SQLite/MySQL/PG + argon2）· 按用户配额（free=50MB / pro=1GiB）· 固定记忆（`nsmt:share_dir` note） |
| M7 | `ygg-admin` 独立监督器（spawn/restart/kill ygg、崩溃自动拉起）+ Web UI（:8090）+ 控制 API 聚合 |
| M8 | 会员（`users.plan` free/pro）· 配额 UI（用量条 + 升级 CTA）· 计费接口预留 |
| M9 | P2P 对等认证 + NAT 打洞 · 冲突合并 Web GUI（`conflicts-web`）· S3 多租户前缀 · E2E 密钥轮换/按租户 · 域池分片 |
| 待办池 | 后台进程重启 · 租户备份/恢复（控制 API） |

---

## 总体架构

```
                       用户域 "ser163"
┌──────────────────────────────────────────────────────────────┐
│  家里电脑 (Machine A)          公司电脑 (Machine B)            │
│  ┌──────────────┐              ┌──────────────┐               │
│  │ agent (maka) │              │ agent(hermes)│               │
│  └──────┬───────┘              └──────┬───────┘               │
│  ┌──────▼────────┐            ┌──────▼────────┐               │
│  │ yggd (client) │◄──────────►│ yggd (client) │  ← Rust 守护进程│
│  │  ├ 腾讯 GW    │            │  ├ 腾讯 GW    │  ← 记忆        │
│  │  ├ nsmt_share │            │  ├ nsmt_share │  ← 文件        │
│  │  └ P2P 监听    │            │  └ P2P 监听    │               │
│  └──────┬────────┘            └──────┬────────┘               │
│         │       QUIC (UDP)           │  P2P 直连               │
│         └──────────────┬─────────────┘                        │
│                        ▼                                     │
│              ┌─────────────────────┐                          │
│              │  ygg (relay server) │  ← Rust 单二进制         │
│              │  registry / 在线     │                          │
│              │  锁 (租约)           │                          │
│              │  对象存储            │  (本地 FS / S3/MinIO)   │
│              │  域记忆池           │  (按租户分片)            │
│              │  用户 / 配额         │  控制 API :8091          │
│              └─────────────────────┘                          │
│              ┌─────────────────────┐                          │
│              │ ygg-admin :8090     │  ← 监督器 + Web UI      │
│              └─────────────────────┘                          │
└──────────────────────────────────────────────────────────────┘
```

## 组件

| 二进制 | crate | 角色 | 端口 |
|---|---|---|---|
| `ygg` | `nsmt-server` | relay + registry + 锁 + 对象存储 + 记忆池 + 用户 + 控制 API | UDP 5555, :8091 |
| `yggd` | `nsmt-client` | 握手、记忆双写/托底、文件同步（CAS/目录树/diff/锁/续传/P2P）、冲突 CLI + Web GUI | P2P 监听, :8088 |
| `ygg-admin` | `nsmt-admin` | 独立监督器 + Web UI + 控制 API 聚合 | :8090 |
| — | `nsmt-core` | 身份、帧编解码、消息、E2E 加密、对等认证帧 | — |
| — | `nsmt-memory` | 腾讯 Gateway HTTP 客户端（recall/capture/search/health） | — |
| — | `nsmt-fs` | ObjectStore 抽象（本地/内存/S3）+ `PrefixedObjectStore` | — |

---

## 快速开始

### 构建

```bash
cargo build --release
# target/release/nsmt-server -> ygg
# target/release/nsmt-client -> yggd
# target/release/nsmt-admin  -> ygg-admin
```

### 运行

```bash
# 1)（可选）腾讯记忆 Gateway
node --import <路径>/src/gateway/server.ts &

# 2) 服务器（带控制 API + 管理 token）
NSMT_ADMIN_TOKEN=secret ./target/release/nsmt-server 0.0.0.0:5555 --control 127.0.0.1:8091

# 3)（可选，M7）监督器 + Web UI
./target/release/nsmt-admin --ygg ./target/release/nsmt-server \
  --control 127.0.0.1:8091 --bind 127.0.0.1:8090 --token secret -- \
  0.0.0.0:5555 --control 127.0.0.1:8091

# 4) 首次注册租户（客户端首次运行自动生成密钥）
./target/release/nsmt-client 127.0.0.1:5555 2>/dev/null || true
ygg admin add-tenant ser163 "$(cat ~/.nsmt/domain.pub)"

# 5) 客户端 A（在线）/ 客户端 B（文件同步，模拟另一台机器）
NSMT_USER_DOMAIN=ser163 NSMT_AGENT_TAG=maka   ./target/release/nsmt-client 127.0.0.1:5555
NSMT_USER_DOMAIN=ser163 NSMT_MACHINE_ID=bbbb1111aaaa0000 \
  NSMT_SHARE_DIR=~/nsmt_share_b ./target/release/nsmt-client 127.0.0.1:5555 fs

# 6) 冲突合并 Web GUI（M9.2）
./target/release/nsmt-client 127.0.0.1:5555 conflicts-web 8088   # 打开 http://127.0.0.1:8088
```

### 记忆命令

```bash
# capture = 双写（域池 + 本地托底）；recall = 网络优先，超时回退本地
yggd 127.0.0.1:5555 capture "用户说" "助手回复"
yggd 127.0.0.1:5555 recall "问题"
```

### 冲突合并

```bash
yggd 127.0.0.1:5555 conflicts                          # 列出冲突副本
yggd 127.0.0.1:5555 merge .sync-conflict-xxx [--keep-local|--keep-remote]
yggd 127.0.0.1:5555 conflicts-web                      # Web GUI（:8088）
```

---

## 文档索引

| 文档 | 内容 |
|---|---|
| `docs/项目策划.md` | 原始需求与设计决策 |
| `ARCHITECTURE.md` | 架构总览 |
| `protocol/protocol.md` | **协议规范（单一事实来源，最重要）** |
| `DEV-ROADMAP.md` | 开发规划 M6–M9 + 待办池 |
| `deploy/` | 部署指南：服务器 / 客户端 / 总览 |

## 测试

```bash
cargo test        # 全部 crate 的单元 + 集成测试
```

## 状态

所有里程碑 **M0–M9** 与**待办池**均已实现 ✅（2026-08-30），并在 macOS 上端到端实测通过
（双机文件同步 + E2E、P2P 直连拉取、后台重启、备份/恢复、会员升级）。详见 `DEV-ROADMAP.md`。

## License

MIT
