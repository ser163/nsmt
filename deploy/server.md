# NSMT Server Deployment (服务器端部署)

> English primary · 中文为辅
> `ygg` — relay server: registry, online list, locks, object store, domain memory pool, quotas, admin.

## 1. Build (构建)

```bash
cargo build --release
cp target/release/nsmt-server /usr/local/bin/ygg   # Linux/macOS
# Windows: copy target\release\nsmt-server.exe to your PATH
```

## 2. Prerequisites (前置)

- UDP port **5555** open (firewall / security group).
- Optional: **MinIO or S3-compatible** object store (decision #10) for the file backend.
- Optional: a **Tencent memory gateway** instance to act as the per-tenant **domain pool**
  (recommended; see `client.md` §2 for the gateway). If absent, memory dual-write/fallback still works
  with local gateways only.

## 3. Configuration (配置)

All via environment variables (see table). Data & keys live under `~/.nsmt/` (`NSMT_HOME`).

| Env | Default | Meaning |
|---|---|---|
| `NSMT_HOME` | `~/.nsmt` | data dir |
| `NSMT_POOL_GATEWAY` | `http://127.0.0.1:8420` | domain pool Tencent gateway |
| `NSMT_POOL_GATEWAYS` | — | M9.5: comma-separated pool shards (fan-out recall, fqn-hash capture) |
| `NSMT_OBJECT_STORE` | `local` | `local` \| `memory` \| `s3` |
| `NSMT_S3_ENDPOINT/BUCKET/REGION/ACCESS_KEY/SECRET_KEY/HTTP` | — | S3/MinIO config (decision #10); keys prefixed `t/<domain>/objects/` per tenant (M9.3) |
| `NSMT_QUOTA_BYTES` | 1 GiB | fallback per-tenant quota (overridden by user plan: free=50MB / pro=1GiB, M8) |
| `NSMT_E2E_KEY` / `NSMT_E2E_KEYS` | — | 32-byte hex key(s); `NSMT_E2E_KEYS` = comma-separated, newest first (rotation M9.4); per-tenant key derived from master + domain |
| `NSMT_DB_URL` | sqlite://~/.nsmt/users.db | user DB (M6): `sqlite://` / `postgres://` / `mysql://` |
| `NSMT_ADMIN_TOKEN` | — | control API & ygg-admin auth (`x-admin-token`) |
| `NSMT_CONTROL_ADDR` | — | control API bind (or `--control ip:port`) |
| `RUST_LOG` | info | `debug` for verbose logs |

### Tenant registration (首次注册租户)

```bash
# 1) let a client generate keys once, read its domain public key:
cat ~/.nsmt/domain.pub
# 2) on the server, register the tenant (do this BEFORE the client connects):
ygg admin add-tenant ser163 <DOMAIN_PUBKEY_HEX>
```

## 4. Run (运行)

> 建议配合 `ygg-admin`（M7 监督器 + Web UI）运行，崩溃自动拉起、Web 管理：

```bash
# supervisor + Web UI (:8090)，控制 API (:8091)
NSMT_ADMIN_TOKEN=secret ./target/release/nsmt-admin \
  --ygg ./target/release/nsmt-server \
  --control 127.0.0.1:8091 --bind 127.0.0.1:8090 --token secret -- \
  0.0.0.0:5555 --control 127.0.0.1:8091
```

### Linux — systemd (recommended 推荐)

```ini
# /etc/systemd/system/ygg.service
[Unit]
Description=NSMT Yggdrasil relay server
After=network.target

[Service]
User=nsmt
ExecStart=/usr/local/bin/ygg 0.0.0.0:5555 --control 127.0.0.1:8091
Environment=NSMT_QUOTA_BYTES=52428800
Environment=NSMT_POOL_GATEWAY=http://127.0.0.1:8420
Environment=NSMT_ADMIN_TOKEN=secret
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now ygg
sudo systemctl status ygg
journalctl -u ygg -f        # 查看日志 view logs
```

> 用 systemd/launchd 时 OS 负责重启，`ygg-admin` 仍可作管理 + 监控 + Web UI（M7 fallback）。

### macOS — launchd (推荐 推荐)

```xml
<!-- ~/Library/LaunchAgents/com.nsmt.ygg.plist -->
<plist version="1.0"><dict>
  <key>Label</key><string>com.nsmt.ygg</string>
  <key>ProgramArguments</key>
  <array><string>/usr/local/bin/ygg</string><string>0.0.0.0:5555</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>EnvironmentVariables</key><dict>
    <key>NSMT_QUOTA_BYTES</key><string>52428800</string>
  </dict>
  <key>StandardOutPath</key><string>/Users/nsmt/.nsmt/logs/ygg.out.log</string>
  <key>StandardErrorPath</key><string>/Users/nsmt/.nsmt/logs/ygg.err.log</string>
</dict></plist>
```

```bash
launchctl load ~/Library/LaunchAgents/com.nsmt.ygg.plist
launchctl kickstart -k gui/$(id -u)/com.nsmt.ygg    # 重启 restart
tail -f ~/.nsmt/logs/ygg.err.log                    # 查看日志
```

### Windows — Task Scheduler / NSSM

```powershell
# NSSM (recommended)
nssm install NSMT-Ygg "C:\nsmt\nsmt-server.exe" "0.0.0.0:5555"
nssm set NSMT-Ygg AppEnvironmentExtra "NSMT_QUOTA_BYTES=52428800"
nssm set NSMT-Ygg AppStdout "C:\nsmt\logs\ygg.out.log"
nssm set NSMT-Ygg AppStderr "C:\nsmt\logs\ygg.err.log"
nssm start NSMT-Ygg
# 或 Task Scheduler：新建任务 → 程序 = nsmt-server.exe，参数 = 0.0.0.0:5555
```

## 5. S3 / MinIO backend (S3 对象存储, 决策 #10 / M9.3 多租户前缀)

```bash
# local MinIO (single binary)
minio server ~/minio-data --address 127.0.0.1:9000 &
mc alias set local http://127.0.0.1:9000 minioadmin minioadmin
mc mb local/nsmt-bucket

# run ygg with S3 backend (对象按 t/<domain>/objects/ 前缀隔离，M9.3)
NSMT_OBJECT_STORE=s3 \
NSMT_S3_ENDPOINT=http://127.0.0.1:9000 \
NSMT_S3_REGION=us-east-1 NSMT_S3_BUCKET=nsmt-bucket \
NSMT_S3_ACCESS_KEY=minioadmin NSMT_S3_SECRET_KEY=minioadmin NSMT_S3_HTTP=true \
ygg 0.0.0.0:5555
```

> M9.3：S3/内存为共享后端，对象 key 自动加 `t/<user_domain>/objects/` 前缀实现多租户隔离；本地后端仍按目录隔离。

## 6. Verification (验证)

```bash
# server up
ygg 0.0.0.0:5555 --control 127.0.0.1:8091 &   # logs "ygg listening on ..."
# client connects (see client.md) → server logs "machine registered"
# control API (M6, admin token)
curl -s -H "x-admin-token: secret" http://127.0.0.1:8091/api/status
# backup / restore (待办池)
curl -s -H "x-admin-token: secret" "http://127.0.0.1:8091/api/backup?domain=ser163"
curl -s -X POST -H "x-admin-token: secret" -H "Content-Type: application/json" \
  http://127.0.0.1:8091/api/restore -d '{"domain":"ser163","archive":"<backup path>"}'
# graceful restart (M7, exit code 3 → ygg-admin respawns)
curl -s -X POST -H "x-admin-token: secret" http://127.0.0.1:8091/api/admin/restart
# check data
ls ~/.nsmt/server/<domain>/objects/    # files synced via local backend
mc ls --recursive local/nsmt-bucket     # files synced via S3 (under t/<domain>/objects/)
```

## 7. Troubleshooting (排障)

| Issue | Check |
|---|---|
| QUIC handshake fails / BadSignature | client must trust server cert `~/.nsmt/ygg.crt` (auto-written; client reads it) |
| AUTH failed / auth_failed | re-register tenant: `ygg admin add-tenant <domain> <pubkey>` (or register via `/api/users/register` + set key) |
| quota_exceeded (0xE040) | raise plan (M8) or `NSMT_QUOTA_BYTES` fallback |
| E2E mismatch (decrypt failed) | `NSMT_E2E_KEY`/`NSMT_E2E_KEYS` must match server & peers; keep old keys during rotation |
| S3 not storing | verify `NSMT_S3_*` + bucket exists + `NSMT_S3_HTTP=true` for http |
| control API unauthorized | set `NSMT_ADMIN_TOKEN` and send `x-admin-token` |
| ygg exits unexpectedly | check exit code; `ygg-admin` restarts automatically (crash) or on code 3 (graceful) |
| port in use | `lsof -iUDP:5555` / `ss -ulpn | grep 5555` |

> 中文说明：端口、租户注册、S3、配额、用户/会员、控制 API、备份恢复、重启见上；多数问题先看 `RUST_LOG=debug` 输出。
