# NSMT Protocol — Yggdrasil 网络协议规范 v0.2

> 项目：Net Share Memory Tree（Yggdrasil）
> 协议版本：0.2
> 传输：QUIC (HTTP/3 over UDP)
> 状态：**已实现**（Windows 全功能验证 + macOS E2E 验证；含文件同步、记忆、P2P、OKF 知识库）
> 本文件是项目**最重要文档**，所有实现以它为准。

---

## 1. 概述

NSMT（Yggdrasil）是一个**多租户、多机器、多 agent 共享记忆与文件**的网络协议。

- **目标**：让不同机器上的 agent（Maka / Hermes / 自定义）接入同一个用户域，共享同一棵记忆树与共享文件，识别记忆归属，带锁协同编辑。
- **传输**：QUIC（UDP），低延迟、多路复用、0-RTT 重连、内置可靠传输。
- **拓扑**：relay 服务器（v1 强制）+ P2P 直连（设计预留）。
- **语言**：参考实现 Rust（`quinn`）。

### 1.1 设计原则

1. **身份即命名空间**：一切资源以 `FQN` 为前缀，服务器只认租户前缀，强制隔离。
2. **记忆主从**：域级公共记忆池为主（权威），本地托底（离线保底）；写双写、读先池后托底。
3. **文件内容寻址**：文件按内容哈希分块（CAS），目录树做增量同步，冲突用版本号 + 冲突副本。
4. **锁**：服务器端租约锁（lease），防并发修改冲突。
5. **向前兼容**：协议带版本号；所有扩展帧走 `reserved` 通道。

### 1.2 术语表

| 术语 | 含义 |
|---|---|
| tenant / user_domain | 用户域，租户边界（如 `ser163`） |
| machine_id | 机器码（硬件级稳定哈希） |
| agent_tag | 机器上的 agent 实例标识（`maka`/`hermes`/自定义） |
| FQN | `<user_domain>/<machine_id>/<agent_tag>` |
| ygg | 服务器（relay + registry + 锁 + 文件/记忆服务） |
| yggd | 客户端 daemon（本机 agent 侧） |
| 域池（domain pool） | 服务器上每租户一个腾讯 Gateway 实例 = 权威共享记忆 |
| 托底（fallback） | 本机记忆库（默认本地腾讯 Gateway），读回退目标 |

---

## 2. 身份与命名空间

### 2.1 FQN（Fully Qualified Name）

```
<user_domain> / <machine_id> / <agent_tag>
   ser163     /   9f2c81d4…  /   maka
```

- `user_domain`：`[a-z0-9][a-z0-9._-]{2,63}`，租户键；
- `machine_id`：16 位小写 hex。生成规则：`SHA-256(IOPlatformUUID || hostname || os_name || os_version)` 取前 16 hex。硬件级稳定；
- `agent_tag`：`[a-zA-Z0-9._-]{1,63}`，用户配置。

### 2.2 密钥

| 密钥 | 级别 | 用途 |
|---|---|---|
| `identity.key` | 机器级 Ed25519 | 机器身份签名、TLS 客户端证书绑定 |
| `domain.key` | 用户域级 Ed25519 | 签发机器凭证、可选数据加密 |

存放：`~/.nsmt/identity.key`、`~/.nsmt/domain.key`（权限 0600）。

### 2.3 鉴权流程

1. 客户端 → 服务器：`HELLO`（携带 `user_domain`，TLS 握手已建立）；
2. 服务器 → 客户端：`HELLO_ACK`（若租户存在，返回 nonce；否则 `ERROR tenant_not_found`）；
3. 客户端 → 服务器：`AUTH`（`user_domain` + 用 `domain.key` 对 nonce 签名）；
4. 服务器 → 客户端：`TICKET`（短期票据，内含 machine 注册所需信息）；
5. 客户端 → 服务器：`REGISTER`（machine_id + agent_tag + 机器签名）；
6. 服务器 → 客户端：`REGISTER_ACK`（含该租户在线机器表快照）。

### 2.4 租户命名空间（所有服务器资源统一前缀）

```
t/<user_domain>/machines/<machine_id>/agents/<agent_tag>
t/<user_domain>/online/<machine_id>
t/<user_domain>/memory                     ← 域池 Gateway 实例标识
t/<user_domain>/objects/<sha256>           ← 文件对象（CAS）
t/<user_domain>/trees/<tree_hash>          ← 目录树快照
t/<user_domain>/locks/<path>               ← 文件锁
t/<user_domain>/queue/<machine_id>         ← 离线待同步队列
```

**隔离规则**：服务器任何 handler 只允许操作「当前认证租户」前缀下的键；客户端自报路径一律先加租户前缀再校验。

---

## 3. 传输层

### 3.1 连接

- 传输：QUIC（HTTP/3）；TLS 1.3 默认加密；
- 默认端口：`ygg` 服务器 `UDP 5555`（QUIC），可配置；
- 客户端连接：`yggd → ygg`；P2P 时 `yggd ↔ yggd`（经服务器信令交换地址后直连，QUIC）。

### 3.2 流（Stream）分配

| Stream 编号 | 用途 |
|---|---|
| 0 | 控制（帧协议） |
| 1 | 心跳 |
| 2 | 记忆（recall/capture） |
| 3 | 文件元数据（tree/diff） |
| 4 | 文件数据（chunk 上传/下载，可并发多条） |
| 5 | 锁 |
| 6+ | 扩展 / 事件广播 |

### 3.3 帧格式（二进制头 + payload）

```
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
| magic = 0x59   | version = 1   | flags         | frame_type    |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
| stream_id (u32 LE)                                            |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
| payload_len (u32 LE)                                          |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
| payload (payload_len bytes)                                   |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

- `magic=0x59`（'Y'，Yggdrasil）；
- `flags`：`bit0=encrypted(E2E)`、`bit1=compress`、`bit2..7=reserved`；
- `frame_type`：见 §4；
- 控制帧 payload 为 **JSON**（可读、可调试）；文件 chunk 为**二进制**；
- 单帧 payload 上限 16 MiB；更大内容用 `STREAM` 分片。

---

## 4. 帧类型

| 值 | 帧 | 方向 | 说明 |
|---|---|---|---|
| 0x01 | `HELLO` | c→s | 握手开始，含 `user_domain` |
| 0x02 | `HELLO_ACK` | s→c | nonce / 租户状态 |
| 0x03 | `AUTH` | c→s | 域签名 |
| 0x04 | `TICKET` | s→c | 短期票据 |
| 0x05 | `REGISTER` | c→s | 机器 + agent 注册 |
| 0x06 | `REGISTER_ACK` | s→c | 注册结果 + 在线机器表快照 |
| 0x10 | `HEARTBEAT` | c⇄s | 心跳（含负载/时间戳） |
| 0x11 | `ONLINE_LIST` | s→c | 全量在线列表 |
| 0x12 | `ONLINE_DELTA` | s→c | 在线增量（join/leave） |
| 0x20 | `MEMORY_RECALL` | c→s | 记忆召回请求 |
| 0x21 | `MEMORY_RECALL_RESULT` | s→c | 召回结果 |
| 0x22 | `MEMORY_CAPTURE` | c→s | 记忆写入（双写） |
| 0x23 | `MEMORY_CAPTURE_RESULT` | s→c | 写入结果 |
| 0x30 | `FILE_TREE` | c⇄s | 目录树（tree hash） |
| 0x31 | `FILE_DIFF` | c→s | 请求差异 |
| 0x32 | `FILE_DIFF_RESULT` | s→c | 路径差异列表 |
| 0x33 | `FILE_GET` | c→s | 请求对象 |
| 0x34 | `FILE_PUT` | c→s | 上传对象（chunk 头） |
| 0x35 | `FILE_CHUNK` | c⇄s | 数据块（二进制） |
| 0x40 | `LOCK_ACQUIRE` | c→s | 申请锁 |
| 0x41 | `LOCK_RENEW` | c→s | 续约 |
| 0x42 | `LOCK_RELEASE` | c→s | 释放 |
| 0x43 | `LOCK_GRANTED` | s→c | 授予 |
| 0x44 | `LOCK_DENIED` | s→c | 拒绝（含持有者） |
| 0x45 | `LOCK_NOTIFY` | s→c | 锁状态广播 |
| 0x50 | `PEER_HELLO` | c⇄c | P2P 对等认证①：自报身份（M9.1） |
| 0x51 | `PEER_AUTH` | c⇄c | P2P 对等认证②：下发 nonce |
| 0x52 | `PEER_AUTH_OK` | c⇄c | P2P 对等认证③：域密钥签名 + 确认 |
| 0x53 | `PEER_HINT` | s→c | 对象 miss 打洞信令：通知持有者 requester 外部地址 |
| 0xF0 | `ERROR` | 任意 | 错误（见 §9） |

---

## 5. 连接生命周期

```
yggd                                    ygg
  │  QUIC connect (TLS 1.3)              │
  ├────────── HELLO ────────────────────▶│
  │◀───────── HELLO_ACK (nonce) ────────┤
  ├────────── AUTH (sign nonce) ────────▶│
  │◀───────── TICKET ───────────────────┤
  ├────────── REGISTER ─────────────────▶│
  │◀───────── REGISTER_ACK (machines) ──┤
  │         （就绪：同步机器表/agent 列表）│
  │◀───────── ONLINE_LIST ──────────────┤
  │═══════ 正常服务（记忆/文件/锁）═══════│
```

- **心跳**：每 10s `HEARTBEAT`；服务器 3 次未收到 → 标记离线 → 广播 `ONLINE_DELTA leave`；
- **断线重连**：QUIC 0-RTT + 旧票据续期，重连后自动补 `ONLINE_LIST` 差异与待同步队列。

### 5.1 P2P 连接与对等认证（M9.1）

P2P 直连（yggd ↔ yggd，经服务器发现地址）在数据交换前先做**应用层对等认证**（替换 dev no-verify TLS 作为信任基础）：

```
A (fetch)                              B (owner)
  │  QUIC connect (TLS 自签)            │
  ├────────── PEER_HELLO ─────────────▶│   { user_domain, machine_id, agent_tag, machine_pubkey }
  │◀────────── PEER_AUTH ──────────────┤   { nonce }
  ├──── PEER_AUTH_OK (域密钥签名) ──────▶│   { machine_id, agent_tag, machine_pubkey, signature }
  │◀────────── PEER_AUTH_OK (确认) ─────┤   B 对同一 nonce 签名，供 A 验证 B
  │═══════ FILE_GET / FILE_CHUNK ═══════│
```

- 双方都用**用户域私钥**（同域共享 `domain.key`）对 nonce 签名，对方用同域公钥验证 → 证明持有域私钥且同域；
- 失败：直接断开，不服务 `FILE_GET`；
- 打洞连接（`PEER_HINT` 触发）只建立 QUIC + 开流即关闭，NAT 映射打开后由后续真实拉取复用。

### 5.2 NAT 打洞（M9.1）

1. 服务器在 `REGISTER` 时记录客户端**外部地址**（`conn.remote_address`）到 `MachineInfo.addr`；
2. 客户端 `FILE_GET` 服务器 miss → 服务器返回 `ERROR object not found; peer=<owner_peer_addr>`，
   并向租户广播 `PEER_HINT { blob_id, requester_addr }`（requester 外部地址）；
3. 持有者收到 `PEER_HINT` 且本地有该对象 → 主动 `hole_punch`（QUIC connect requester_addr，开流即关）打开 NAT 映射；
4. requester 随后直连 owner（peer_addr 或 addr 任一可达）→ 走 §5.1 对等认证 → 拉取。

---

## 6. 记忆协议

### 6.1 命名空间映射

| 概念 | 映射 |
|---|---|
| 本机记忆 | 本地腾讯 Gateway，`instanceId = machine_id` |
| 域池记忆 | 服务器上该租户的 Gateway 实例（`t/<user_domain>/memory`） |
| 域池分片（M9.5） | `NSMT_POOL_GATEWAYS` 逗号分隔多网关；recall fan-out 聚合、capture 按 fqn 哈希路由 |
| scope=user | 双写：域池 + 本地 |
| scope=machine/agent | 只写本地 |

### 6.2 `MEMORY_RECALL`

```json
{
  "request_id": "uuid",
  "query": "…",
  "scope": "user|machine|agent",
  "limit": 5,
  "timeout_ms": 1500,
  "fallback_on_timeout": true
}
```

服务器行为（`scope=user`）：
1. 转发到**域池** Gateway `/recall`（M9.5 分片模式：fan-out 到所有分片并发查询，按内容稳定排序截断 top `limit`）；
2. **成功** → 返回 `MEMORY_RECALL_RESULT`（标注 `source=pool`）；全部 shard 失败 → `source=pool_unavailable`；
3. **超时/失败** → 回退：若 `fallback_on_timeout`，向该客户端返回 `MEMORY_RECALL_RESULT`（`source=local` 指令，客户端转查本地托底）；或服务器直接返回错误码，由客户端本地兜底。

`MEMORY_RECALL_RESULT`：

```json
{
  "request_id": "uuid",
  "source": "pool|local|relay",
  "memories": [
    { "content": "…", "fqn": "ser163/9f2c…/maka", "score": 0.85, "scope": "user" }
  ],
  "latency_ms": 42
}
```

### 6.3 `MEMORY_CAPTURE`（双写）

```json
{
  "request_id": "uuid",
  "user_content": "…",
  "assistant_content": "…",
  "scope": "user",
  "fqn": "ser163/9f2c…/maka",
  "observed_at": 1788000000000
}
```

- 服务器：写域池（主）→ 返回 `MEMORY_CAPTURE_RESULT { committed: true }`；
- 客户端同时写本地托底；
- **离线**：客户端只写本地，条目进 `t/<user_domain>/queue/<machine_id>`，重连后补写池；
- **幂等**：`observed_at + content_hash` 去重，重复 capture 不产生重复向量条目。
- **固定记忆约定（M6.4）**：客户端首启把共享目录位置写入一条系统 note，`user_content` = `[nsmt:share_dir] 共享目录: <abs path>`、`assistant_content` = `(system)`、`scope` = `user`；同时写本地托底并落 marker `~/.nsmt/share.path`（写一次即不再重复，离线首启重试），任何 agent `recall` 共享目录均可定位。

### 6.4 待同步队列（离线补写）

- 队列条目：`{ fqn, payload, queued_at }`；
- 重连成功后按序补写；成功即出队；失败保留重试（指数退避）。

---

## 7. 文件协议

### 7.1 对象模型

```
对象 blob_id = SHA-256(content)（16 进制）
目录树 tree = 有序映射 { path → (blob_id, mode, size, mtime_ns) }
           → 对映射整体做 SHA-256 得 tree_hash
```

### 7.2 虚拟共享目录

- 客户端 `~/nsmt_share/`（决策 #3）；
- `nsmt-share/<path>` 是指向本地 CAS 缓存 `.nsmt/objects/<sha256>` 的 **symlink**；
- 未下载的对象先建占位 symlink，后台按需拉取补齐（决策 #4）。

### 7.3 同步流程

1. 客户端 A 变更文件 → 写本地 CAS + 更新本地 tree；
2. A → 服务器 `FILE_PUT`（对象，chunk 分片 `FILE_CHUNK`）+ `FILE_TREE`（新 tree_hash）；
3. 服务器更新 `t/<domain>/trees/<tree_hash>`，并向其它在线客户端推送 `ONLINE` 事件或让它们下次 `FILE_DIFF` 拉取；
4. 客户端 B 收到提示 → `FILE_DIFF`（old_tree → new_tree）→ 服务器返回差异 → B 按需 `FILE_GET` 补齐缺失对象；
5. **启动先拉后推（M9 修正）**：客户端 fs 模式启动先 `FILE_DIFF` 并拉取远端变更，再推本地树 —— 空目录不会覆盖共享树；
6. 服务器 miss（对象缺失）→ 返回 `peer=` 提示 + 广播 `PEER_HINT`（见 §5.2）→ B 直连持有者拉取（对等认证）。

### 7.3.1 对象存储与多租户前缀（M9.3）

| 后端 | 租户隔离方式 |
|---|---|
| 本地 FS | 根目录 `~/.nsmt/server/<user_domain>/objects/` 天然隔离 |
| S3 / 内存（共享后端） | key 加前缀 **`t/<user_domain>/objects/`**（`PrefixedObjectStore`），同一 bucket 内按租户隔离 |

### 7.4 断点续传

- 对象按固定 chunk（如 4 MiB）分片，各自 `blob_id + chunk_index`；
- `FILE_GET` 可指定 `chunk_index`；客户端记录已收 chunk，断线续传。

### 7.5 锁（乐观并发 + 租约）

`LOCK_ACQUIRE`：

```json
{
  "path": "docs/plan.md",
  "requester": "ser163/9f2c…/maka",
  "ttl_ms": 30000,
  "op": "write"
}
```

- 服务器授予并记录租约（`locks/<path>`），返回 `LOCK_GRANTED`；
- 续约：`LOCK_RENEW` 每 10s；超 TTL 自动释放；
- 冲突：版本号兜底 —— 写入时若 `tree` 中该文件版本已被他人更新 → 保留冲突副本 `.sync-conflict-<machine>-<ts>` + `LOCK_NOTIFY` 通知相关方；
- 服务器重启：租约持久化 SQLite，恢复后过期锁自动清理。

---

## 8. 在线状态与发现

- `HEARTBEAT` 间隔 10s；服务器维护 `t/<domain>/online/<machine_id> → {addr, last_seen, agents:[]}`；
- 变化即广播 `ONLINE_DELTA`（`join` / `leave` / `agent_change`）；
- 新客户端 `REGISTER_ACK` 携带当前全量在线表。

---

## 9. 错误码

| 码 | 含义 |
|---|---|
| 0xE001 | tenant_not_found |
| 0xE002 | auth_failed |
| 0xE003 | ticket_expired |
| 0xE004 | not_registered |
| 0xE005 | tenant_forbidden（越权访问他租户命名空间） |
| 0xE010 | memory_pool_unavailable |
| 0xE011 | memory_recall_timeout |
| 0xE012 | memory_capture_conflict（幂等冲突） |
| 0xE020 | object_not_found |
| 0xE021 | tree_conflict（版本冲突） |
| 0xE030 | lock_held（已被他人持有） |
| 0xE031 | lock_timeout |
| 0xE040 | quota_exceeded |
| 0xE041 | rate_limited |
| 0xE050 | peer_auth_failed（P2P 对等认证失败，M9.1） |
| 0xE0FF | internal_error |

`ERROR` 帧：

```json
{ "code": "0xE005", "message": "tenant forbidden", "request_id": "uuid" }
```

---

## 10. 安全

- 传输：QUIC/TLS 1.3（默认加密）；
- 租户隔离：服务器强制命名空间前缀校验（见 §2.4）+ 集成测试；
- 可选 E2E：`flags.bit0` 开启，payload 用租户密钥派生密钥加密；
  - **按租户密钥（M9.4）**：`derive_tenant_key(master, domain) = SHA-256("nsmt:e2e:v1:" || domain || master)`，
    server/client 同域派生一致，无需网络分发；
  - **密钥轮换（M9.4）**：`NSMT_E2E_KEYS`（逗号分隔，最新在前）；加密用最新，解密尝试全部，
    旧密钥保留即可解密历史数据；新密钥加到列表头并重启两端完成轮换；
- **P2P 对等认证（M9.1）**：P2P 连接建立后先做应用层认证（`PEER_HELLO → PEER_AUTH → PEER_AUTH_OK`），
  双方用**用户域私钥**对 nonce 签名，同域机器彼此可验（共享 domain key），替换 dev no-verify TLS 作为信任基础；
- **NAT 打洞（M9.1）**：服务器观测各机器外部地址；对象 miss 时除返回 `peer=<peer_addr>` 提示外，
  还向持有者广播 `PEER_HINT`（含 requester 外部地址），持有者主动 `hole_punch` 打开 NAT 映射；
- **S3 多租户（M9.3）**：共享对象后端（S3/内存）按租户加前缀 `t/<domain>/objects/`，本地后端按根目录隔离；
- **域池分片（M9.5）**：`NSMT_POOL_GATEWAYS` 逗号分隔多网关；recall fan-out 聚合、capture 按 fqn 哈希路由；
- 票据：短期 + 绑定机器 + 可吊销（`ygg admin revoke`）。

---

## 11. 版本与演进

- 协议版本号 `version=1`（帧头）；
- 兼容策略：服务器拒绝不支持的旧版本（`ERROR unsupported_version`）；新增帧走 `reserved` 通道，向后兼容；
- 重大变更：升版本号 + 双端协商降级。

---

## 12. 实现清单（对照）

- [x] 帧编解码（magic/version/flags/type/stream/len）
- [x] FQN 解析 + 机器码生成
- [x] 鉴权（HELLO/AUTH/REGISTER）+ 用户系统（M6，sqlx Any + argon2）
- [x] 心跳 + ONLINE_LIST/DELTA
- [x] MEMORY_RECALL/CAPTURE（域池 + 托底 + 固定记忆 note + 分片 fan-out）
- [x] FILE_TREE/DIFF/PUT/GET/CHUNK（CAS + symlink + 断点续传 + S3 多租户前缀）
- [x] LOCK 全套（租约 + 续约 + 冲突副本）
- [x] P2P 对等认证 + NAT 打洞（PEER_* 帧 + PeerHint）
- [x] 冲突合并（CLI + Web GUI `conflicts-web`）
- [x] E2E 加密（按租户密钥 + 轮换）
- [x] ERROR 表 + 租户隔离测试
- [x] 管理面：控制 API（M6）/ ygg-admin 监督器 + Web UI（M7）/ 会员配额（M8）/ 备份恢复（待办池）

---

## 13. 管理面（HTTP 控制 API，M6+）

> 管理面走 **HTTP/JSON**（非 QUIC 帧），`ygg --control 127.0.0.1:8091` 启用；
> 鉴权：`NSMT_ADMIN_TOKEN` → 请求头 `x-admin-token`。`ygg-admin`（:8090）为监督器 + Web UI。

| 端点 | 方法 | 说明 |
|---|---|---|
| `/api/status` | GET | 健康/uptime/pid/配额/用量 |
| `/api/tenants` | GET/POST | 租户列表 + 用量 / 添加租户（域+公钥） |
| `/api/online` | GET | 在线机器/agent |
| `/api/locks` | GET | 锁状态 |
| `/api/logs?lines=N[&filter=]` | GET | 日志 tail |
| `/api/users/register` | POST | 自助注册（argon2，自动建租户） |
| `/api/users/login` | POST | 登录发 token |
| `/api/users` | GET | 用户列表 + plan + 用量 + 配额（M8 UI 数据源） |
| `/api/users/{username}/upgrade` | POST | 会员升级 `{plan: free\|pro}`（M8） |
| `/api/tenants/key` | POST | 用户登记客户端域公钥 |
| `/api/admin/restart` | POST | 优雅重启（退出码 3，由 ygg-admin 拉起，M7） |
| `/api/backup?domain=` | GET | 打包租户数据到 `backups/`（待办池） |
| `/api/restore` | POST | 从备份恢复租户（待办池） |

> 冲突合并 Web GUI 为**客户端本地**服务：`yggd conflicts-web [port]`（默认 127.0.0.1:8088）。
