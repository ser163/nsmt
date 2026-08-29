# NSMT / Yggdrasil — Deployment Guide (部署指南)

> English primary · 中文为辅
> Covers: **Linux / macOS / Windows** — server (`ygg`) and client (`yggd`).
> Detailed steps: `server.md` (server) · `client.md` (client) · `ARCHITECTURE.md` (architecture) · `protocol/protocol.md` (protocol).

## Overview (总览)

| Component | Binary | Role | Default Ports |
|---|---|---|---|
| Server | `ygg` | relay + registry + locks + object store + domain memory pool + admin | UDP 5555 (QUIC), :9000 (MinIO if S3) |
| Client | `yggd` | agent-side daemon (memory + file sync + P2P) | P2P listener (ephemeral) |
| Memory engine | Tencent Gateway (Node) | L0–L3 memory per machine / domain pool | 127.0.0.1:8420 |

## Requirements (前置要求)

- **Rust toolchain** ≥ 1.85 to build (or use a prebuilt binary if provided).
- **Node.js** ≥ 22.16 — only needed if you use the TencentDB memory engine (recommended).
- **A reachable UDP port** for QUIC (default 5555).
- Optional: **MinIO / S3-compatible** object store for the file backend (decision #10).
- Optional: local proxy for CN networks when downloading deps/models.

## Build (构建)

```bash
git clone <repo-url> && cd nsmt
cargo build --release
# binaries:
#   target/release/nsmt-server   -> ygg
#   target/release/nsmt-client   -> yggd
```

> 国内网络：为 cargo 设置代理（如 `export HTTPS_PROXY=socks5h://127.0.0.1:7890`），或配置 `~/.cargo/config.toml` 走镜像。

## Quick Start (快速开始, 本机验证)

```bash
# 1) (optional) start the Tencent memory gateway on 127.0.0.1:8420
node --import <path>/src/gateway/server.ts &

# 2) server
./target/release/nsmt-server 0.0.0.0:5555

# 3) client A (online mode)
NSMT_USER_DOMAIN=ser163 NSMT_AGENT_TAG=maka ./target/release/nsmt-client 127.0.0.1:5555

# 4) client B (file sync mode, another "machine")
NSMT_USER_DOMAIN=ser163 NSMT_AGENT_TAG=hermes NSMT_MACHINE_ID=bbbb1111aaaa0000 \
  NSMT_SHARE_DIR=~/nsmt_share_b ./target/release/nsmt-client 127.0.0.1:5555 fs
```

## First-time tenant registration (首次租户注册)

```bash
# client generates keys on first run: ~/.nsmt/domain.key|.pub, machine.key|.pub
# read the domain public key and register the tenant on the server:
ygg admin add-tenant ser163 "$(cat ~/.nsmt/domain.pub)"
```

## Platform notes (平台说明)

- **Linux**: systemd recommended for `ygg` (see `server.md`). Prefer `Linux: aarch64/x86_64`.
- **macOS**: launchd via `launchctl` (example in `server.md`). arm64 build.
- **Windows**: run as a background service via Task Scheduler / NSSM; PowerShell-friendly commands.

## Documents (文档索引)

| Doc | Content |
|---|---|
| `server.md` | Server deployment (Linux/macOS/Windows) — systemd / launchd / NSSM, S3, quotas, admin |
| `client.md` | Client deployment & startup — env vars, commands, memory & file usage |
| `ARCHITECTURE.md` | Architecture overview |
| `protocol/protocol.md` | Wire protocol spec (most important) |
| `DEV-ROADMAP.md` | Web admin / user system / membership / fixed-memory roadmap |

---

*Deployment verified on: macOS 26.6 (Apple Silicon), Node 24, MinIO.*
