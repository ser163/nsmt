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

- Conflict merge GUI (web/桌面).
- P2P NAT hole punching + peer authentication (replace dev no-verify TLS).
- S3 实机多租户（每个 user 一个 bucket prefix）.
- E2E key rotation & per-tenant keys.
- Memory vector DB 跨机聚合优化（域池分片）.
- Admin web: process restart, backup/restore of tenants.

## 7. Task Breakdown & Milestones (任务清单)

### M6 — ygg Control API + User System (进行中 ✅ in progress)

- [x] **M6.1 ygg 控制 API ✅**（`ygg --control 127.0.0.1:8091`，axum 内嵌 HTTP，2026-08-30 实测通过）：
  - [x] `GET /api/status` 服务器健康/uptime/pid/配额/用量
  - [x] `GET /api/tenants` 租户列表+用量；`POST /api/tenants` 添加租户（域+公钥）
  - [x] `GET /api/online` 在线机器/agent；`GET /api/locks` 锁状态
  - [x] `GET /api/logs?lines=N` 日志 tail（NSMT_LOG_FILE）
  - [x] 控制 API 鉴权（NSMT_ADMIN_TOKEN / x-admin-token）
- [x] **M6.2 用户系统 ✅**：**通用 SQL 驱动 sqlx Any（SQLite/MySQL/PostgreSQL，`NSMT_DB_URL` 换库即用）**；`users/sessions` 表；注册→登录→自动建租户（domain=username）；argon2 密码；`POST /api/users/register|login`、`POST /api/tenants/key`
- [x] **M6.3 配额按用户 ✅**：`users.plan`（free=50MB / pro=1GiB）→ `ServerState.quota_for(domain)`；注册响应返回 quota_bytes=52428800；无用户库时回退全局 env
- [ ] **M6.4 固定记忆**：客户端首次运行把共享目录写入共享记忆（note, key=`nsmt:share_dir`）

### M7 — ygg-admin (独立监督器 + Web UI) (planned)

- [ ] **M7.1 ygg-admin 监督器**：spawn/restart/kill ygg 子进程；健康/CPU/内存监控；崩溃自动拉起
- [ ] **M7.2 Web UI**：状态页（进程/在线/用量）、租户管理、日志查看（tail + 过滤）
- [ ] **M7.3 控制 API 客户端**：ygg-admin 轮询 ygg 控制 API 聚合展示

### M8 — Membership & Quotas UI (planned)

- [ ] `users.plan` = free(50MB) / pro(1GiB) …；配额 UI（用量条 + 升级 CTA）
- [ ] 计费接口预留（Stripe/微信/支付宝），暂不接入

### M9 — Hardening (planned)

- [ ] P2P 打洞 + 对等认证（替换 dev no-verify TLS）
- [ ] 冲突合并 GUI（Web 页面对话式合并）
- [ ] S3 多租户（每用户 bucket prefix）
- [ ] E2E 密钥轮换 & 按租户密钥
- [ ] 域池向量库跨机聚合优化

> 优先级：M6（控制 API + 用户系统）→ M7（ygg-admin）→ M8（会员）→ M9（加固）。
