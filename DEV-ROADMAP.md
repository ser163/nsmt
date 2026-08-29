# NSMT / Yggdrasil — Development Roadmap (开发规划)

> English primary · 中文为辅
> 本文记录待开发功能的需求与设计，供后续实现。

## 1. Web Admin Console (Web 管理后台)

**需求**：一个 Web 后台，用于管理服务器、监控进程、查看日志。

**Requirements**:
- **Server management**: start / stop / restart `ygg`; view config; tenant CRUD (create / delete / revoke machine).
- **Monitoring**: live process status (PID, uptime, CPU/mem), online machines & agents, active connections, per-tenant usage (storage / bandwidth / memory count).
- **Logs**: browse `ygg` logs (stdout/stderr) with filters + level; follow (tail).
- **Auth**: admin login (password / token), separate from user-facing API.
- **Design decision (2026-08-30)**: **two processes** — a separate `ygg-admin` supervisor + a **control API** inside `ygg`. A process cannot cleanly restart/observe itself, so the admin must be out-of-process to survive crashes and restart the server.
  ```
  ygg-admin (Web UI + supervisor, :8090)
     ├── spawn / restart / kill ygg          (process management)
     ├── monitor health / CPU / mem / logs   (tail log files)
     └── HTTP control API 127.0.0.1:8091 → ygg (status/tenants/quotas/online/locks/usage)
  ```
- **`ygg` control API** (`--control 127.0.0.1:8091`): read-only status + admin ops + log stream.
- **Fallback**: on systemd/launchd deploys, OS may supervise restarts; `ygg-admin` still handles management + monitoring + UI.
- **API sketch**:
  ```
  GET  /api/status                 # server health + uptime
  GET  /api/tenants                # list tenants + usage
  POST /api/tenants                # create tenant (invite code)
  POST /api/tenants/{id}/revoke    # revoke a machine
  GET  /api/online                 # online machines
  GET  /api/logs?tail=200          # recent logs
  ```

> 中文：后台用 Rust 提供 HTTP API + 内嵌前端；支持登录、租户管理、在线监控、日志查看。

## 2. User System & Self-Registration (用户系统与自助注册)

**需求**：支持多用户；用户可以自己注册并使用；需要有用户管理数据库。

**Requirements**:
- **User DB**: per-server SQLite (e.g., `~/.nsmt/users.db`) — tables: `users`, `sessions`, `invites`, `usage`.
  ```
  users(id, username UNIQUE, password_hash, domain, created_at, plan)
  sessions(token, user_id, expires_at)
  invites(code, role, used_by, expires_at)
  usage(user_id, storage_bytes, updated_at)
  ```
- **Auth flow**: register (username + password, optionally invite code for private beta) → login → JWT/session token → call `ygg admin add-tenant`-equivalent automatically (tenant = username domain).
- **Password hashing**: argon2.
- **Rate limit** on register/login.

> 中文：服务器内建 SQLite 用户库；注册（可带邀请码）→ 登录 → 自动创建租户；密码 argon2。

## 3. Per-User Quota (默认用户配额)

**需求**：默认用户共享目录大小限制 **50 MB**；以后会员可扩容。

**Requirements**:
- Default quota per user: **50 MB** (`NSMT_DEFAULT_QUOTA_BYTES=52428800`).
- Enforcement: existing `NSMT_QUOTA_BYTES` per-tenant check (FILE_PUT pre-reserve) — wire it to the user's `plan` instead of a global env.
- Usage accounting: persist `usage.storage_bytes` so it survives restarts.

> 中文：默认 50 MB/用户；配额校验已实现（0xE040），改为按用户 plan 读取。

## 4. Membership / Paid Plans (会员与扩容, future)

**需求**：以后考虑会员功能，可扩容配额。

**Requirements**:
- `users.plan` = `free | pro | ...`; quota resolved from plan (free=50 MB, pro=1 GiB …).
- Optional billing hooks (Stripe / 微信支付 / 支付宝) — keep an interface, defer integration.
- Web console shows current usage vs quota + upgrade CTA.

> 中文：plan 字段驱动配额；计费接口预留，暂不接入。

## 5. Fixed Memory: Shared Directory Location (固定记忆)

**需求**：记忆里应该有固定记忆，记录共享目录的位置。

**Requirements**:
- The shared directory path (`NSMT_SHARE_DIR`, default `~/nsmt_share`) must be a **well-known, persistent memory entry** so agents can always locate it.
- Implementation options:
  - (a) A constant/env that agents read (`NSMT_SHARE_DIR`) — already exists.
  - (b) Write it into the shared memory as a fixed system entry: on first client run, `capture` a `system note`:
    `共享目录: <abs path>` (scope=user, kind=note, key=`nsmt:share_dir`) so any agent can recall it.
  - (c) Persist a local marker file `~/.nsmt/share.path` + memory entry.
- Recommended: **both (a) env and (b) memory entry** — client writes the note once at startup if absent, so memory recalls return the path reliably.

> 中文：共享目录位置 = 固定记忆（env + 首次运行写入一条 note 到共享记忆），保证任何 agent 都能找到。

## 6. Backlog (待办池)

- [x] Conflict merge GUI (web/桌面) — ✅ M9.2 `yggd conflicts-web`
- [x] P2P NAT hole punching + peer authentication — ✅ M9.1
- [x] S3 实机多租户（每个 user 一个 bucket prefix）— ✅ M9.3
- [x] E2E key rotation & per-tenant keys — ✅ M9.4
- [x] Memory vector DB 跨机聚合优化（域池分片）— ✅ M9.5
- [x] Admin web: process restart, backup/restore of tenants — ✅ M7.2 + 控制 API backup/restore

## 7. Task Breakdown & Milestones (任务清单)

### M6 — ygg Control API + User System (已完成 ✅)

- [x] **M6.1 ygg 控制 API ✅**（`ygg --control 127.0.0.1:8091`，axum 内嵌 HTTP，2026-08-30 实测通过）：
  - [x] `GET /api/status` 服务器健康/uptime/pid/配额/用量
  - [x] `GET /api/tenants` 租户列表+用量；`POST /api/tenants` 添加租户（域+公钥）
  - [x] `GET /api/online` 在线机器/agent；`GET /api/locks` 锁状态
  - [x] `GET /api/logs?lines=N` 日志 tail（NSMT_LOG_FILE）
  - [x] 控制 API 鉴权（NSMT_ADMIN_TOKEN / x-admin-token）
- [x] **M6.2 用户系统 ✅**：**通用 SQL 驱动 sqlx Any（SQLite/MySQL/PostgreSQL，`NSMT_DB_URL` 换库即用）**；`users/sessions` 表；注册→登录→自动建租户（domain=username）；argon2 密码；`POST /api/users/register|login`、`POST /api/tenants/key`
- [x] **M6.3 配额按用户 ✅**：`users.plan`（free=50MB / pro=1GiB）→ `ServerState.quota_for(domain)`；注册响应返回 quota_bytes=52428800；无用户库时回退全局 env
- [x] **M6.4 固定记忆 ✅**（2026-08-30 实测通过）：客户端首启把共享目录绝对路径写入**共享记忆**（经服务器 MEMORY_CAPTURE，域池 note，key=`nsmt:share_dir`）+ 本地托底（双写）+ marker `~/.nsmt/share.path`（落盘后不再重复写，离线首启可重试）；任何 agent `recall` 共享目录都能拿到路径

### M7 — ygg-admin (独立监督器 + Web UI) (已完成 ✅)

- [x] **M7.1 ygg-admin 监督器 ✅**：新 crate `nsmt-admin`；spawn/restart/kill ygg 子进程；健康/CPU/内存监控（`ps` 采样）；崩溃自动拉起（指数退避）；`POST /api/restart`（优先经 ygg 控制 API 优雅重启，退出码 3 约定，失败则 kill）
- [x] **M7.2 Web UI ✅**：内嵌状态页（进程/在线/用量/租户/用户配额/日志 tail）、租户管理、日志查看（行数+过滤）、备份/恢复
- [x] **M7.3 控制 API 客户端 ✅**：ygg-admin 轮询/代理 ygg 控制 API（status/tenants/online/locks/logs/users/backup/restore）

### M8 — Membership & Quotas UI (已完成 ✅)

- [x] **会员/配额 API ✅**：`users.plan`（free=50MB / pro=1GiB）→ 配额按 plan；`GET /api/users`（列表+用量+配额条）、`POST /api/users/{u}/upgrade`（admin 升级）
- [x] **配额 UI ✅**：ygg-admin Web 用量条 + 升级 CTA；计费接口预留（`set_plan` 为接入点，Stripe/微信/支付宝 后续接入）

### M9 — Hardening (已完成 ✅)

- [x] **M9.1 P2P 打洞 + 对等认证 ✅**：P2P 应用层对等认证（PeerHello/PeerAuth/PeerAuthOk，域密钥签名，替换 no-verify TLS 作为信任基础）；服务器对象 miss 时广播 PeerHint（含 requester 外部地址），持有者主动打洞（hole_punch 打开 NAT 映射）
- [x] **M9.2 冲突合并 GUI ✅**：`yggd conflicts-web [port]` 内嵌 axum Web 页（对话式：对比本地/远端、三选一解决：保留本地/远端/自定义合并）
- [x] **M9.3 S3 多租户 ✅**：`PrefixedObjectStore` 按租户 `t/<domain>/objects/` 前缀隔离（S3/内存共享后端）
- [x] **M9.4 E2E 密钥轮换 & 按租户密钥 ✅**：`NSMT_E2E_KEYS`（逗号分隔多密钥，最新在前，加密用最新、解密尝试全部）；`derive_tenant_key(master, domain)` 按租户派生，无需网络分发
- [x] **M9.5 域池向量库跨机聚合 ✅**：`NSMT_POOL_GATEWAYS`（逗号分隔多分片）；recall fan-out 聚合（按内容排序截断 top limit）、capture 按 fqn 哈希路由单分片

> 优先级：M6（控制 API + 用户系统）→ M7（ygg-admin）→ M8（会员）→ M9（加固）。
