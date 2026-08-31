# NSMT (Yggdrasil) Windows 部署文档

> 适用：Windows 10/11，本机实测环境（Rust 1.97 / git-bash / Docker 可选）
> 版本：nsmt master (f57cdaf 起含 Windows 补丁)
> 部署对象：`ygg`（服务器）+ `yggd`（客户端）+ `ygg-admin`（管理台）

---

## 第 1 部分：环境准备与编译

### 1.1 前置要求

| 组件 | 版本要求 | 说明 |
|------|---------|------|
| Rust 工具链 | ≥ 1.80（实测 1.97） | `rustup` 安装，MSVC target |
| git | 任意 | 拉取代码（国内需代理：`git config http.proxy http://127.0.0.1:7892`） |
| 记忆引擎（可选） | TencentDB Agent Memory gateway | 本机 127.0.0.1:8420，NSMT 的记忆后端；不装则仅文件同步可用 |

### 1.2 拉取与编译

```bash
# 1) 拉取（国内网络建议带代理）
git clone https://github.com/ser163/nsmt.git
cd nsmt

# 2) 编译（首次约 10-20 分钟，产物 3 个 exe）
cargo build --release
```

### 1.3 ⚠️ Windows 补丁（上游 f57cdaf 前必打）

上游仅在 macOS 验证过，Windows 编译有 2 处平台代码问题。**修复已提交**（本仓库 f57cdaf），若从上游重新拉取需手动打：

| 文件 | 原代码（编译失败） | 修复 |
|------|-------------------|------|
| `crates/nsmt-client/src/fs.rs:139` | `std::os::unix::fs::symlink(...)` | 改调 cfg 双分支的 `make_symlink()`（Windows 用 `std::os::windows::fs::symlink_file`） |
| `crates/nsmt-admin/src/main.rs:210` | `terminate()` 仅有 `#[cfg(unix)]` 分支 | 补 `#[cfg(windows)]` 分支（`taskkill /PID <pid> /T /F`） |

### 1.4 产物

```text
target/release/nsmt-server.exe   → ygg（relay 服务器，~15 MB）
target/release/nsmt-client.exe   → yggd（客户端 daemon，~10 MB）
target/release/nsmt-admin.exe    → ygg-admin（监督器 + Web 管理台，~7.5 MB）
```

建议复制到 `C:\nsmt\` 并加入 PATH。

---

## 第 2 部分：服务器端部署（ygg）

### 2.1 首次启动 + 租户注册

```bash
# 1) 启动服务器（控制 API 开在 8091，管理 token 自定）
NSMT_ADMIN_TOKEN=secret C:\nsmt\nsmt-server.exe 0.0.0.0:5555 --control 127.0.0.1:8091
# 日志出现 "ygg listening on 0.0.0.0:5555 (QUIC)" 即成功

# 2) 让任意一台客户端先跑一次生成密钥（连接会失败，属正常）
#    （见第 3 部分 3.1，先执行那里的步骤 1）

# 3) 用客户端生成的域公钥注册租户
C:\nsmt\nsmt-server.exe admin add-tenant <你的域名> <domain.pub 内容>
# 输出 "tenant added: <域名>" 即成功
```

> ⚠️ **租户表无热重载**：add-tenant 之后必须**重启服务器**（启动时只加载一次 `~/.nsmt/tenants.json`）。

### 2.2 运行验证

```bash
# 健康检查
curl -s -H "x-admin-token: secret" http://127.0.0.1:8091/api/status
# 期望: {"pid":..., "status":"ok", "tenants":1}

# 在线机器列表
curl -s -H "x-admin-token: secret" http://127.0.0.1:8091/api/online
```

### 2.3 服务化（NSSM，开机自启）

```powershell
nssm install NSMT-Ygg "C:\nsmt\nsmt-server.exe" "0.0.0.0:5555"
nssm set NSMT-Ygg AppEnvironmentExtra "NSMT_ADMIN_TOKEN=secret NSMT_QUOTA_BYTES=1073741824"
nssm set NSMT-Ygg AppStdout "C:\nsmt\logs\ygg.out.log"
nssm set NSMT-Ygg AppStderr "C:\nsmt\logs\ygg.err.log"
nssm start NSMT-Ygg
```

### 2.4 防火墙

- **UDP 5555 入站**必须放行（QUIC 传输端口，客户端连接用）
- 控制 API 8091 建议仅绑 127.0.0.1（`--control 127.0.0.1:8091`），不对外
- Web 管理台 8090（ygg-admin）如需远程访问再放行

---

## 第 3 部分：客户端部署（yggd）

### 3.1 首次运行（生成密钥）

```bash
# 首次运行自动生成 ~/.nsmt/ 下 domain.key / machine.key / ygg.crt
# 连接会因租户未注册而失败，但密钥已生成
NSMT_USER_DOMAIN=<域名> NSMT_AGENT_TAG=<标签> C:\nsmt\nsmt-client.exe <服务器IP>:5555
cat %USERPROFILE%\.nsmt\domain.pub    # 把内容交给服务器管理员注册租户
```

### 3.2 环境变量

| 变量 | 必填 | 默认 | 说明 |
|------|------|------|------|
| `NSMT_USER_DOMAIN` | ✅ | `ser163` | 用户域（租户），须与服务器注册一致 |
| `NSMT_AGENT_TAG` | ✅ | `maka` | 本机 agent 标识 |
| `NSMT_SHARE_DIR` | 文件模式 | `~/nsmt_share` | 共享目录。**Windows 用正斜杠**：`C:/Users/<你>/nsmt_share` |
| `NSMT_MACHINE_ID` | 多机测试 | 自动(硬件哈希) | 单机模拟多机时覆盖 |
| `NSMT_SERVER_CERT` | 自动 | `~/.nsmt/ygg.crt` | 信任服务器证书（首连服务器自动写入） |
| `NSMT_PEER_PORT` | P2P | 临时端口 | 跨 NAT 直连需固定端口并在可达接口监听 |
| `NSMT_E2E_KEY` | 可选 | — | 32 字节 hex；开启 E2E 加密（server/client/peer 必须一致） |
| `RUST_LOG` | 可选 | info | `debug` 看详细日志 |

### 3.3 启动模式

```bash
# 在线模式（心跳 + 在线列表）
NSMT_USER_DOMAIN=<域名> NSMT_AGENT_TAG=<标签> nsmt-client.exe <服务器IP>:5555

# 文件同步模式（watch + 双向同步 + P2P）
NSMT_SHARE_DIR=C:/Users/<你>/nsmt_share nsmt-client.exe <服务器IP>:5555 fs

# 记忆命令（一次性）
nsmt-client.exe <服务器IP>:5555 capture "用户说" "助手回复"
nsmt-client.exe <服务器IP>:5555 recall "问题"

# 冲突合并（GUI）
nsmt-client.exe <服务器IP>:5555 conflicts-web 8088   # http://127.0.0.1:8088
```

### 3.4 服务化（NSSM）

```powershell
nssm install NSMT-Yggd "C:\nsmt\nsmt-client.exe" "<服务器IP>:5555" "fs"
nssm set NSMT-Yggd AppEnvironmentExtra "NSMT_USER_DOMAIN=<域名> NSMT_AGENT_TAG=<标签> NSMT_SHARE_DIR=C:/Users/<你>/nsmt_share"
nssm start NSMT-Yggd
```

### 3.5 OKF 知识包接口（Open Knowledge Format v0.2）

共享目录可直接作为 **OKF 知识包（bundle）** 使用——纯 Markdown + YAML frontmatter 的开放格式（Google Cloud 发布，Apache-2.0）。`yggd okf` 提供创建/校验/索引命令，文件仍走 NSMT 正常同步/锁/冲突处理：

```bash
# bundle 根：默认 NSMT_SHARE_DIR；可用 NSMT_OKF_ROOT 覆盖

# 初始化 + 创建概念（type 为唯一必填字段，自动生成 frontmatter 模板）
yggd okf init
yggd okf new tables/orders.md --type "BigQuery Table" --title "Customer Orders" \
      --description "One row per completed order" --tags sales,orders

# 校验符合性（每个 .md 须有 frontmatter + 非空 type；bad.md 会报错退出码 1）
yggd okf validate

# 浏览 / 生成目录索引 / 展示 / 记录变更
yggd okf list [--type T]
yggd okf index            # 按目录生成 index.md（§8）
yggd okf show <path>      # frontmatter + 正文预览
yggd okf log "<message>"  # 追加 log.md（§9）
```

生成的 concept 示例：

```markdown
---
type: BigQuery Table
title: Customer Orders
description: One row per completed order
tags: [sales, orders]
status: draft
generated: { by: process:nsmt, at: 2026-08-31T05:43:01Z }
---

# Customer Orders
```

任何 OKF 消费者（Agent、工具）均可直接读取共享目录；`index.md`/`log.md` 为保留文件名（§3.1），自动生成。

### 3.6 排障速查

| 现象 | 检查 |
|------|------|
| `tenant_not_found` | 租户注册了吗？注册后**重启服务器**了吗？ |
| `handshake` 失败 | `~/.nsmt/ygg.crt` 是否存在（服务器先起一次）？UDP 5555 通吗？ |
| 文件不同步 | 两端都要 `fs` 模式；`NSMT_SHARE_DIR` 必须是**正斜杠 Windows 绝对路径**（反斜杠会被 shell 转义吃掉）；看 `RUST_LOG=debug` 的 `diff:` 日志 |
| E2E 解密失败 | `NSMT_E2E_KEY`/`NSMT_E2E_KEYS` 与服务器/对端不一致 |
| 记忆不可用 | 本地 Tencent gateway 127.0.0.1:8420 是否运行；`/health` 里 `embeddingService` 是否 true |

---

## 第 4 部分：端口与服务器配置

### 4.1 端口规划

| 端口 | 协议 | 组件 | 用途 | 是否对外 |
|------|------|------|------|---------|
| **5555** | UDP (QUIC) | ygg 服务器 | 客户端主连接、P2P 信令 | ✅ 必须（公网部署需映射） |
| 8091 | TCP | ygg 控制 API | 管理 API（status/tenants/online/locks/logs） | ❌ 仅 127.0.0.1 |
| 8090 | TCP | ygg-admin | Web 管理台（监督器 + 配额 UI） | 按需 |
| 8088 | TCP | yggd conflicts-web | 冲突合并 Web GUI（客户端本地） | ❌ 仅 127.0.0.1 |
| — | TCP | yggd P2P 监听 | 对等直连（`NSMT_PEER_PORT` 固定后） | ✅ NAT 打洞需可达 |

### 4.2 服务器配置项（环境变量）

| 变量 | 默认 | 说明 |
|------|------|------|
| `NSMT_HOME` | `~/.nsmt` | 数据目录（tenants.json / 对象存储 / 证书） |
| `NSMT_POOL_GATEWAY` | `http://127.0.0.1:8420` | 域池 Tencent gateway（共享记忆权威源） |
| `NSMT_POOL_GATEWAYS` | — | 逗号分隔多分片网关（M9.5 fan-out） |
| `NSMT_OBJECT_STORE` | `local` | `local` / `memory` / `s3` |
| `NSMT_S3_ENDPOINT/BUCKET/REGION/ACCESS_KEY/SECRET_KEY/HTTP` | — | S3/MinIO 后端；对象按 `t/<domain>/objects/` 前缀租户隔离 |
| `NSMT_QUOTA_BYTES` | 1 GiB | 租户配额兜底（用户库激活后按 plan：free 50MB / pro 1GiB） |
| `NSMT_E2E_KEY` / `NSMT_E2E_KEYS` | — | E2E 加密密钥；KEYS 逗号分隔最新在前（轮换） |
| `NSMT_DB_URL` | `sqlite://~/.nsmt/users.db` | 用户库。**⚠️ Windows 上 `~` 不展开，必须显式传绝对路径**：`sqlite:///C:/Users/<你>/.nsmt/users.db` |
| `NSMT_ADMIN_TOKEN` | — | 控制 API 鉴权（请求头 `x-admin-token`） |

### 4.3 公网部署（互联网多机共享）

```
[机器A yggd] ──QUIC/UDP──> [ygg 服务器:5555] <──QUIC/UDP── [机器B yggd]
                  (公网 IP / 端口映射 5555)
```

- 路由器把 **UDP 5555** 映射到服务器内网 IP（或直接用云主机安全组放行）
- 客户端连接地址写公网 IP/域名
- ⚠️ 安全注意：NSMT 无 TURN 兜底，跨运营商/对称 NAT 下 P2P 打洞可能失败；文件走服务器中转时注意配额
- 生产建议：`NSMT_ADMIN_TOKEN` 必须设置；控制 API 不对外

### 4.4 内存记忆拓扑（推荐组合）

```
每台机器: 本地 Tencent gateway (127.0.0.1:8420) = 本地托底
服务器:   域池 gateway (NSMT_POOL_GATEWAY)      = 共享权威
写入:     capture 双写（池 + 本地）→ 断网不丢
读取:     recall 先池后本地 → 断网可回退
```
