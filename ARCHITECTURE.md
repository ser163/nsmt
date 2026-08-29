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
│              │  quotas / admin     │                          │
│              └─────────────────────┘                          │
└──────────────────────────────────────────────────────────────┘
```

## 4. Components (组件)

| Crate | Role |
|---|---|
| `nsmt-core` | identity (FQN/Ed25519), frame codec, messages, error codes, E2E crypto |
| `nsmt-server` (`ygg`) | QUIC relay, registry, online list, locks, object store, memory pool bridge, quotas, tenants, admin CLI |
| `nsmt-client` (`yggd`) | handshake, memory dual-write/fallback, file sync (CAS/tree/diff/lock/resume/P2P), conflict CLI |
| `nsmt-memory` | Tencent Gateway HTTP client (recall/capture/search/health) |
| `nsmt-fs` | ObjectStore abstraction (Local / Memory / S3) |

## 5. Identity & Tenancy (身份与租户)

- **FQN**: `<user_domain>/<machine_id>/<agent_tag>`; `machine_id` = SHA-256(hardware UUID + hostname + OS) [16 hex].
- **Keys**: machine-level & domain-level Ed25519 (`~/.nsmt/domain.key`, `machine.key`); AUTH signs a per-connection nonce with the domain key; REGISTER signs `machine_id\nagent_tag` with the machine key.
- **Tenant isolation**: every server key is `t/<user_domain>/…`; the server rejects cross-tenant access (`0xE005`).
- **Quota**: per-tenant storage cap (`NSMT_QUOTA_BYTES`, default 1 GiB; product default 50 MB per user, see DEV-ROADMAP).

## 6. Transport & Protocol (传输与协议)

- QUIC (quinn, rustls); TLS 1.3; optional E2E (ChaCha20-Poly1305, `NSMT_E2E_KEY`, flags bit0).
- Binary frames: `magic 0x59 | version 1 | flags | type | stream | len | payload`.
- Stream 0 = control; frames: HELLO/AUTH/REGISTER/HEARTBEAT/ONLINE, MEMORY_*, FILE_* (tree/diff/put/get/chunk/ack), LOCK_*.
- Detail: **`protocol/protocol.md`** (single source of truth).

## 7. Memory Model (记忆模型)

- **Domain pool (authoritative)** — a Tencent Gateway instance per tenant on the server (`t/<domain>/memory`).
- **Local fallback** — each machine's local Tencent Gateway (`127.0.0.1:8420`).
- `capture` → **dual write** (pool + local); offline → local + queued resync.
- `recall` → **network-first** (pool), **timeout → local fallback**; results tagged `pool|local` + FQN.

## 8. File Model (文件模型)

- **CAS objects**: blob_id = SHA-256(content); stored under `~/.nsmt/server/<domain>/objects/` (local) or S3.
- **Directory trees**: path → (blob_id, mode, size, mtime); tree_hash = SHA-256 over sorted entries; incremental diff.
- **Sync**: push objects + tree; peers pull via diff; conflict copies `.sync-conflict-*`; lease locks (30 s TTL, 10 s renew).
- **Resume**: 1 MiB chunks, `FilePutAck.have` resumes; `FILE_GET` by chunk_index.
- **P2P**: clients run a listener; server records object owners; on server miss → peer address hint → direct fetch.

## 9. Security (安全)

- QUIC/TLS 1.3 transport; optional E2E payload encryption; Ed25519 domain/machine auth; tenant namespace enforcement; quota/rate limiting.

## 10. Milestones (里程碑)

| Phase | Status |
|---|---|
| M0 skeleton (identity/QUIC/online) | ✅ |
| M1 memory (dual-write/fallback) | ✅ |
| M2 files (CAS/tree/lock/symlink) | ✅ |
| M3 hardening (auth/conflict/ObjectStore) | ✅ |
| M4 network (resume/S3/P2P/conflict CLI) | ✅ |
| M5 production (MinIO/quota/E2E/interactive merge) | ✅ |
| M6+ (web admin, user system, membership) | planned → see `DEV-ROADMAP.md` |

## 11. Deployment

- See **`deploy/`** for Linux / macOS / Windows server & client deployment (English + 中文).
