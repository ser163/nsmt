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
| `NSMT_OBJECT_STORE` | `local` | `local` \| `memory` \| `s3` |
| `NSMT_S3_ENDPOINT/BUCKET/REGION/ACCESS_KEY/SECRET_KEY/HTTP` | — | S3/MinIO config (decision #10) |
| `NSMT_QUOTA_BYTES` | 1 GiB | per-tenant file storage quota (product default 50 MB — see DEV-ROADMAP) |
| `NSMT_E2E_KEY` | — | 32-byte hex key; enables payload encryption |
| `RUST_LOG` | info | `debug` for verbose logs |

### Tenant registration (首次注册租户)

```bash
# 1) let a client generate keys once, read its domain public key:
cat ~/.nsmt/domain.pub
# 2) on the server, register the tenant (do this BEFORE the client connects):
ygg admin add-tenant ser163 <DOMAIN_PUBKEY_HEX>
```

## 4. Run (运行)

### Linux — systemd (recommended 推荐)

```ini
# /etc/systemd/system/ygg.service
[Unit]
Description=NSMT Yggdrasil relay server
After=network.target

[Service]
User=nsmt
ExecStart=/usr/local/bin/ygg 0.0.0.0:5555
Environment=NSMT_QUOTA_BYTES=52428800
Environment=NSMT_POOL_GATEWAY=http://127.0.0.1:8420
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

## 5. S3 / MinIO backend (S3 对象存储, 决策 #10)

```bash
# local MinIO (single binary)
minio server ~/minio-data --address 127.0.0.1:9000 &
mc alias set local http://127.0.0.1:9000 minioadmin minioadmin
mc mb local/nsmt-bucket

# run ygg with S3 backend
NSMT_OBJECT_STORE=s3 \
NSMT_S3_ENDPOINT=http://127.0.0.1:9000 \
NSMT_S3_REGION=us-east-1 NSMT_S3_BUCKET=nsmt-bucket \
NSMT_S3_ACCESS_KEY=minioadmin NSMT_S3_SECRET_KEY=minioadmin NSMT_S3_HTTP=true \
ygg 0.0.0.0:5555
```

## 6. Verification (验证)

```bash
# server up
ygg 0.0.0.0:5555 &            # logs "ygg listening on ..."
# client connects (see client.md) → server logs "machine registered"
# check data
ls ~/.nsmt/server/<domain>/objects/    # files synced via local backend
mc ls --recursive local/nsmt-bucket     # files synced via S3
```

## 7. Troubleshooting (排障)

| Issue | Check |
|---|---|
| QUIC handshake fails / BadSignature | client must trust server cert `~/.nsmt/ygg.crt` (auto-written; client reads it) |
| AUTH failed / auth_failed | re-register tenant: `ygg admin add-tenant <domain> <pubkey>` |
| quota_exceeded (0xE040) | raise `NSMT_QUOTA_BYTES` |
| S3 not storing | verify `NSMT_S3_*` + bucket exists + `NSMT_S3_HTTP=true` for http |
| port in use | `lsof -iUDP:5555` / `ss -ulpn | grep 5555` |

> 中文说明：端口、租户注册、S3、配额、日志路径见上表；多数问题先看 `RUST_LOG=debug` 输出。
