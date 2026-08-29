# NSMT / Yggdrasil — Architecture

> 架构说明（Architecture Overview）。英文为主（English primary），中文为辅（Chinese notes）。
> 配套文档：协议规范见 `protocol/protocol.md`；部署见 `deploy/`；开发规划见 `DEV-ROADMAP.md`。

## 1. What is NSMT? (NSMT 是什么)

**NSMT (Net Share Memory Tree, codename Yggdrasil)** is a multi-tenant, multi-machine, multi-agent
network layer that shares **memory** and **files** across agents (Maka / Hermes / custom) running on
different machines, under one **user domain**, with identity, locking, resume, P2P, encryption and
quotas.

NSMT 是一个多租户、多机器、多 agent 的「共享记忆 + 共享文件」网络层：不同机器上的 agent 在同一用户域下共享记忆与文件，支持身份鉴权、文件锁、断点续传、P2P、加密与配额。

## 2. Design Principles (设计原则)

1. **Identity is the namespace** — every resource is prefixed by `FQN` (`<user_domain>/<machine_id>/<agent_tag>`); the server enforces tenant isolation.
2. **Reuse, don't rewrite** — memory intelligence is delegated to the existing TencentDB Agent Memory Gateway; NSMT only handles networking / identity / files / sync in Rust.
3. **Fast transport** — QUIC (HTTP/3 over UDP): low latency, multiplexing, 0-RTT resume.
4. **Layered memory** — domain pool (authoritative) + local fallback (offline cache); write dual, read pool-first.
5. **Content-addressed files** — CAS objects + directory trees + lease locks + resume + conflict copies.
6. **Multi-tenant SaaS-ready** — per-tenant namespaces, quotas, admin CLI, optional S3 backend.

## 3. High-Level Architecture (总体架构)

```
                       User Domain "ser163"
┌──────────────────────────────────────────────────────────────┐
│  Machine A (home)              Machine B (work)               │
│  ┌──────────────┐              ┌──────────────┐               │
│  │ agent (maka) │              │ agent(hermes)│               │
│  └──────┬───────┘              └──────┬───────┘               │
│  ┌──────▼────────┐            ┌──────▼────────┐               │
│  │ yggd (client) │◄──────────►│ yggd (client) │  ← Rust daemon│
│  │  ├ Tencent GW │            │  ├ Tencent GW │  ← memory    │
│  │  ├ nsmt_share │            │  ├ nsmt_share │  ← files     │
│  │  └ P2P listener            │  └ P2P listener              │
│  └──────┬────────┘            └──────┬────────┘              │
│         │       QUIC (UDP)           │  P2P direct            │
│         └──────────────┬─────────────┘                        │
│                        ▼                                     │
│              ┌─────────────────────┐                          │
│              │   ygg (relay server)│  ← Rust, single binary  │
│              │  registry / online  │                          │
│              │  locks (lease)      │                          │
│              │  object store       │  (local FS / S3/MinIO)  │
│              │  domain memory pool │  (Tencent GW per tenant)│
│              │  quotas / users     │                          │
│              │  control API :8091  │                          │
│              └─────────────────────┘                          │
│              ┌─────────────────────┐                          │
│              │ ygg-admin :8090     │  ← supervisor + Web UI  │
│              │  spawn/restart ygg  │                          │
│              └─────────────────────┘                          │
└──────────────────────────────────────────────────────────────┘
```

## 4. Components (组件)

| Crate | Role |
|---|---|
| `nsmt-core` | identity (FQN/Ed25519), frame codec, messages, error codes, E2E crypto (rotation/per-tenant), peer-auth frames |
| `nsmt-server` (`ygg`) | QUIC relay, registry, online list, locks, object store (per-tenant prefix), memory pool (sharded), quotas, users (M8), control API, backup/restore |
| `nsmt-client` (`yggd`) | handshake, memory dual-write/fallback, file sync (CAS/tree/diff/lock/resume/P2P), peer auth, conflict CLI + Web GUI |
| `nsmt-admin` (`ygg-admin`) | out-of-process supervisor (spawn/restart/kill ygg, crash auto-restart), Web UI, control-API aggregation |
| `nsmt-memory` | Tencent Gateway HTTP client (recall/capture/search/health) |
| `nsmt-fs` | ObjectStore abstraction (Local / Memory / S3), `PrefixedObjectStore` for multi-tenant |

## 5. Identity & Tenancy (身份与租户)

- **FQN**: `<user_domain>/<machine_id>/<agent_tag>`; `machine_id` = SHA-256(hardware UUID + hostname + OS) [16 hex].
- **Keys**: machine-level & domain-level Ed25519 (`~/.nsmt/domain.key`, `machine.key`); AUTH signs a per-connection nonce with the domain key; REGISTER signs `machine_id\nagent_tag` with the machine key.
- **P2P peer auth (M9.1)**: peers prove domain membership by signing a nonce with the shared domain key (PEER_HELLO → PEER_AUTH → PEER_AUTH_OK), replacing no-verify TLS as the trust basis.
- **Tenant isolation**: every server key is `t/<user_domain>/…`; the server rejects cross-tenant access (`0xE005`); shared object backends (S3/memory) are isolated by the `t/<domain>/objects/` prefix (M9.3).
- **Quota**: per-user quota by `users.plan` (free=50 MB / pro=1 GiB, M8), fallback `NSMT_QUOTA_BYTES` (default 1 GiB).

## 6. Transport & Protocol (传输与协议)

- QUIC (quinn, rustls); TLS 1.3; optional E2E (ChaCha20-Poly1305, flags bit0).
- **E2E keys (M9.4)**: `NSMT_E2E_KEYS` = comma-separated key list (newest first); encrypt with newest, decrypt tries all (rotation); per-tenant key derived via `SHA-256("nsmt:e2e:v1:" || domain || master)` — no on-wire key distribution.
- Binary frames: `magic 0x59 | version 1 | flags | type | stream | len | payload`.
- Stream 0 = control; frames: HELLO/AUTH/REGISTER/HEARTBEAT/ONLINE, MEMORY_*, FILE_* (tree/diff/put/get/chunk/ack), LOCK_*, PEER_* (hello/auth/auth_ok/hint).
- **NAT hole punching (M9.1)**: on object miss the server returns the owner's peer hint and broadcasts `PEER_HINT` (requester's observed address); the owner actively punches the NAT mapping.
- Detail: **`protocol/protocol.md`** (single source of truth).

## 7. Memory Model (记忆模型)

- **Domain pool (authoritative)** — Tencent Gateway per tenant (`t/<domain>/memory`); **sharded (M9.5)**: `NSMT_POOL_GATEWAYS` = comma-separated pool list; recall fans out and aggregates, capture routes by FQN hash to one shard.
- **Local fallback** — each machine's local Tencent Gateway (`127.0.0.1:8420`).
- `capture` → **dual write** (pool + local); offline → local + queued resync.
- `recall` → **network-first** (pool), **timeout → local fallback**; results tagged `pool|local` + FQN.

## 8. File Model (文件模型)

- **CAS objects**: blob_id = SHA-256(content); stored under `~/.nsmt/server/<domain>/objects/` (local) or S3 (per-tenant prefix `t/<domain>/objects/`, M9.3).
- **Directory trees**: path → (blob_id, mode, size, mtime); tree_hash = SHA-256 over sorted entries; incremental diff.
- **Sync**: push objects + tree; peers pull via diff; conflict copies `.sync-conflict-*`; lease locks (30 s TTL, 10 s renew). Client startup does **pull-before-push** so an empty dir never clobbers the shared tree.
- **Resume**: 1 MiB chunks, `FilePutAck.have` resumes; `FILE_GET` by chunk_index.
- **P2P (M9.1)**: clients run a listener with **application-layer peer auth** (domain-key signed nonce); server records object owners + observed external addr; on server miss → peer hint + `PEER_HINT` broadcast → owner punches NAT → direct fetch.
- **Conflict merge GUI (M9.2)**: `yggd conflicts-web [port]` serves a local Web page for side-by-side merge (keep local / keep remote / custom).

## 9. Security (安全)

- QUIC/TLS 1.3 transport; optional E2E payload encryption (**per-tenant derived key + rotation**, M9.4); Ed25519 domain/machine auth + **P2P peer auth** (M9.1); tenant namespace enforcement (incl. S3 prefix isolation, M9.3); quota/rate limiting.
- Admin control API & `ygg-admin` guarded by `NSMT_ADMIN_TOKEN` (`x-admin-token`).
- `ygg-admin` is an **out-of-process supervisor** — a process cannot cleanly restart/observe itself (M7).

## 10. Milestones (里程碑)

| Phase | Status |
|---|---|
| M0 skeleton (identity/QUIC/online) | ✅ |
| M1 memory (dual-write/fallback) | ✅ |
| M2 files (CAS/tree/lock/symlink) | ✅ |
| M3 hardening (auth/conflict/ObjectStore) | ✅ |
| M4 network (resume/S3/P2P/conflict CLI) | ✅ |
| M5 production (MinIO/quota/E2E/interactive merge) | ✅ |
| M6 control API / user system / per-user quota / fixed memory | ✅ |
| M7 ygg-admin (supervisor + Web UI + control aggregation) | ✅ |
| M8 membership & quotas UI (free/pro, upgrade API) | ✅ |
| M9 hardening (P2P auth+hole punch, conflict Web GUI, S3 tenant prefix, E2E rotation/per-tenant, pool sharding) | ✅ |
| Backlog (admin restart/backup/restore of tenants) | ✅ |

## 11. Deployment

- See **`deploy/`** for Linux / macOS / Windows server & client deployment (English + 中文).
- `ygg-admin`: spawn/restart/kill `ygg`, monitor CPU/mem, crash auto-restart, Web UI on `:8090`.
