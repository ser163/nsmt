# NSMT Client Deployment & Startup (客户端部署与启动)

> English primary · 中文为辅
> `yggd` — agent-side daemon: connects to `ygg`, provides **shared memory** (dual-write/fallback)
> and **shared files** (CAS/tree/lock/resume/P2P), plus conflict CLI.

## 1. Build (构建)

```bash
cargo build --release
cp target/release/nsmt-client /usr/local/bin/yggd     # Linux/macOS
# Windows: copy target\release\nsmt-client.exe to PATH
```

## 2. Prerequisites: Memory engine (记忆引擎前置)

Install the TencentDB Agent Memory gateway (L0–L3) so the machine has a local memory library:

```bash
# 1) install the gateway package (see TencentDB-Agent-Memory repo / memory-tencentdb)
# 2) start it on 127.0.0.1:8420 (or let Hermes/OpenClaw auto-start it)
node --import <path>/node_modules/tsx/dist/loader.mjs <path>/src/gateway/server.ts &
curl -s http://127.0.0.1:8420/health     # expect stores.embeddingService true
```

> 若不需要记忆功能，可跳过；仅文件同步仍可用（此时 recall 回退本地失败不阻塞文件功能）。

## 3. Configuration (配置)

All via environment variables.

| Env | Default | Meaning |
|---|---|---|
| `NSMT_USER_DOMAIN` | `ser163` | your user domain (tenant) |
| `NSMT_AGENT_TAG` | `maka` | this agent's tag on this machine |
| `NSMT_MACHINE_ID` | auto (hardware hash) | override for testing multiple machines |
| `NSMT_SERVER_CERT` | `~/.nsmt/ygg.crt` | trust the server's TLS cert |
| `NSMT_SHARE_DIR` | `~/nsmt_share` | shared filesystem view (虚拟共享目录) |
| `NSMT_OBJECTS_DIR` | `~/.nsmt/objects` | local object cache |
| `NSMT_POOL_GATEWAY` | `http://127.0.0.1:8420` | (via server) domain pool |
| `NSMT_LOCAL_GATEWAY` | `http://127.0.0.1:8420` | local fallback memory |
| `NSMT_E2E_KEY` | — | 32-byte hex; must match server & peers for encrypted sync |
| `NSMT_SYMLINK_VIEW` | — | set to `1` to materialize share files as symlinks (on-demand pull) |
| `NSMT_PEER_PORT` | `127.0.0.1:0` | P2P listener address (0 = ephemeral port) |
| `RUST_LOG` | info | debug for verbose |

**Keys** are auto-generated on first run: `~/.nsmt/domain.key|.pub`, `~/.nsmt/machine.key|.pub`.

## 4. First-run & tenant registration (首次运行与租户注册)

```bash
# 1) generate keys (first run; even if connect fails, keys are written)
yggd 127.0.0.1:5555 2>/dev/null || true
cat ~/.nsmt/domain.pub      # give this to the server admin:
#    ygg admin add-tenant <YOUR_DOMAIN> <PASTE_PUBKEY>
# 2) now connect (AUTH will pass)
```

## 5. Startup modes (启动模式)

### Online mode (在线模式) — heartbeat + online list

```bash
NSMT_USER_DOMAIN=ser163 NSMT_AGENT_TAG=maka yggd 127.0.0.1:5555
```

### Memory commands (记忆命令)

```bash
# capture a turn (dual-write: domain pool + local fallback)
yggd 127.0.0.1:5555 capture "user said" "assistant replied"

# recall (network-first; timeout → local fallback)
yggd 127.0.0.1:5555 recall "question"
```

### File sync mode (文件同步模式) — watch + sync + P2P

```bash
NSMT_SHARE_DIR=~/nsmt_share yggd 127.0.0.1:5555 fs
# 1) write files in ~/nsmt_share on machine A
# 2) they sync to machine B's ~/nsmt_share within ~5s (poll)
# 3) conflicts are kept as .sync-conflict-* ; resolve via CLI below
```

### Conflict CLI (冲突合并)

```bash
yggd 127.0.0.1:5555 conflicts                          # list conflict copies
yggd 127.0.0.1:5555 merge .sync-conflict-xxx           # show both versions
yggd 127.0.0.1:5555 merge .sync-conflict-xxx --keep-local
yggd 127.0.0.1:5555 merge .sync-conflict-xxx --keep-remote
# interactive: run merge without flag → choose [l]ocal / [r]emote / [c]ancel
```

## 6. Run as background service (后台运行)

### Linux — systemd

```ini
# /etc/systemd/system/yggd.service
[Unit] Description=NSMT client After=network.target
[Service] User=harry
Environment=NSMT_USER_DOMAIN=ser163 Environment=NSMT_AGENT_TAG=maka
Environment=NSMT_SHARE_DIR=/home/harry/nsmt_share
ExecStart=/usr/local/bin/yggd 127.0.0.1:5555 fs
Restart=always
[Install] WantedBy=multi-user.target
```

### macOS — launchd

```xml
<!-- ~/Library/LaunchAgents/com.nsmt.yggd.plist -->
<dict>
  <key>Label</key><string>com.nsmt.yggd</string>
  <key>ProgramArguments</key>
  <array><string>/usr/local/bin/yggd</string><string>127.0.0.1:5555</string><string>fs</string></array>
  <key>EnvironmentVariables</key><dict>
    <key>NSMT_USER_DOMAIN</key><string>ser163</string>
    <key>NSMT_AGENT_TAG</key><string>maka</string>
    <key>NSMT_SHARE_DIR</key><string>/Users/harry/nsmt_share</string>
  </dict>
  <key>RunAtLoad</key><true/><key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>/Users/harry/.nsmt/logs/yggd.out.log</string>
  <key>StandardErrorPath</key><string>/Users/harry/.nsmt/logs/yggd.err.log</string>
</dict>
```

### Windows — NSSM

```powershell
nssm install NSMT-Yggd "C:\nsmt\nsmt-client.exe" "127.0.0.1:5555" "fs"
nssm set NSMT-Yggd AppEnvironmentExtra "NSMT_USER_DOMAIN=ser163 NSMT_AGENT_TAG=maka NSMT_SHARE_DIR=C:\nsmt\share"
nssm start NSMT-Yggd
```

## 7. Verification (验证)

```bash
# online list / registration
yggd 127.0.0.1:5555                    # prints "registered <machine> online=N"
# memory
yggd 127.0.0.1:5555 capture "hi" "hello"; yggd 127.0.0.1:5555 recall "hi"
# files
echo hi > ~/nsmt_share/a.txt; sleep 6; ls ~/nsmt_share   # synced on other machine
```

## 8. Troubleshooting (排障)

| Issue | Check |
|---|---|
| `read server cert failed` | start `ygg` first; it writes `~/.nsmt/ygg.crt` |
| AUTH failed | tenant registered? `ygg admin add-tenant` |
| files not syncing | both in `fs` mode; `RUST_LOG=debug`; check server `diff:` logs |
| E2E mismatch | `NSMT_E2E_KEY` must match server & peers |

> 中文说明：首次运行自动生成密钥；先注册租户再连接；文件模式用 `fs`；冲突用 `conflicts`/`merge`。
