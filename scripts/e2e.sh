#!/usr/bin/env bash
# NSMT 一键端到端测试（E2E smoke test）
#
# 用法:
#   scripts/e2e.sh [--release] [--keep]
#
# 覆盖场景:
#   1) 服务器 + 控制 API 启动
#   2) 租户注册 + 双机文件同步（含 E2E 加密）
#   3) P2P 直连（服务器 miss → peer hint → 直连对端）
#   4) 冲突合并 Web GUI (conflicts-web) API
#   5) ygg-admin 监督器（优雅重启 + kill -9 自动拉起）
#   6) M8 用户注册/升级/配额
#   7) 租户备份 / 恢复 (control API)
#
# 环境变量可覆盖: NSMT_E2E_KEYS / NSMT_USER_DOMAIN / NSMT_ADMIN_TOKEN
set -euo pipefail

PROFILE=${1:-debug}
KEEP=${2:-}
if [ "$PROFILE" = "--release" ]; then PROFILE=release; shift || true; fi
if [ "${1:-}" = "--keep" ]; then KEEP=1; fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/$PROFILE"
WORK="$(mktemp -d /tmp/nsmt-e2e.XXXXXX)"
H="$WORK/home/.nsmt"
SA="$WORK/shareA"; SB="$WORK/shareB"
SERVER_PORT=16666; CONTROL_PORT=16691; ADMIN_PORT=16690; GUI_PORT=16888
DOMAIN="${NSMT_USER_DOMAIN:-ser163}"
TOKEN="${NSMT_ADMIN_TOKEN:-e2esecret}"
export NSMT_E2E_KEYS="${NSMT_E2E_KEYS:-$(python3 -c "print('ab'*32)")}"
export NSMT_HOME="$H"
export NSMT_ADMIN_TOKEN="$TOKEN"
export NSMT_DB_URL="sqlite://$H/users-e2e.db?mode=rwc"
export NSMT_SERVER_CERT="$H/ygg.crt"
PIDS=()

cleanup() {
  [ -n "${KEEP}" ] && return
  for p in "${PIDS[@]:-}"; do kill -9 "$p" 2>/dev/null || true; done
  rm -rf "$WORK"
}
trap cleanup EXIT

say()  { printf '\n\033[1;36m==> %s\033[0m\n' "$*"; }
ok()   { printf '\033[1;32m  ✓ %s\033[0m\n' "$*"; }
fail() { printf '\033[1;31m  ✗ %s\033[0m\n' "$*"; exit 1; }

mkdir -p "$H" "$SA" "$SB"
echo "测试目录: $WORK  二进制: $BIN"

# ── 0. 密钥生成 + 租户注册 ─────────────────────────────
say "0) 生成密钥并注册租户"
"$BIN/nsmt-client" 127.0.0.1:$SERVER_PORT conflicts 2>/dev/null || true
"$BIN/nsmt-server" admin add-tenant "$DOMAIN" "$(cat "$H/domain.pub")" >/dev/null
ok "tenant registered: $DOMAIN"

# ── 1. 服务器 + 控制 API ──────────────────────────────
say "1) 启动服务器（QUIC:$SERVER_PORT 控制API:$CONTROL_PORT）"
"$BIN/nsmt-server" 127.0.0.1:$SERVER_PORT --control 127.0.0.1:$CONTROL_PORT > "$WORK/server.log" 2>&1 &
SERVER_PID=$!
PIDS+=($SERVER_PID)
sleep 2
curl -sf -H "x-admin-token: $TOKEN" "http://127.0.0.1:$CONTROL_PORT/api/status" >/dev/null \
  && ok "control API /api/status" || fail "control API 未就绪"

# ── 2. 双机文件同步（A 推 → B 拉，E2E 加密）────────────
say "2) 双机文件同步（A→服务器→B，E2E 加密）"
echo "hello from A $(date +%s)" > "$SA/base.txt"
NSMT_MACHINE_ID=aaaaaaaaaaaaaaaa NSMT_SHARE_DIR="$SA" NSMT_PEER_PORT=127.0.0.1:25001 \
  timeout 6 "$BIN/nsmt-client" 127.0.0.1:$SERVER_PORT fs > "$WORK/A.log" 2>&1 || true
grep -q "initial sync done: 1 entries" "$WORK/A.log" && ok "A 推送 1 文件" || fail "A 推送失败"
NSMT_MACHINE_ID=bbbbbbbbbbbbbbbb NSMT_SHARE_DIR="$SB" NSMT_PEER_PORT=127.0.0.1:25002 \
  timeout 8 "$BIN/nsmt-client" 127.0.0.1:$SERVER_PORT fs > "$WORK/B.log" 2>&1 || true
grep -q "initial sync done: 1 entries" "$WORK/B.log" && ok "B 拉取 1 文件" || fail "B 拉取失败"
[ -f "$SB/base.txt" ] && ok "B 共享目录含 base.txt" || fail "B 缺 base.txt"

# ── 3. P2P 直连（删服务器对象 → peer hint → 直连 A）────
say "3) P2P 直连（对等认证 + 服务器 miss 直连）"
NSMT_MACHINE_ID=aaaaaaaaaaaaaaaa NSMT_SHARE_DIR="$SA" NSMT_PEER_PORT=127.0.0.1:25001 \
  nohup "$BIN/nsmt-client" 127.0.0.1:$SERVER_PORT fs > "$WORK/A-live.log" 2>&1 &
PIDS+=($!); sleep 2
echo "p2p direct $(date +%s)" > "$SA/p2p.txt"; sleep 3
BLOB=$(python3 -c "
import json
d=json.load(open('$H/server/$DOMAIN/trees/latest.json'))
print(next(e['blob_id'] for e in d['entries'] if e['path']=='p2p.txt'))
")
find "$H/server" -name "$BLOB" -delete
NSMT_MACHINE_ID=bbbbbbbbbbbbbbbb NSMT_SHARE_DIR="$SB" NSMT_PEER_PORT=127.0.0.1:25002 \
  timeout 12 "$BIN/nsmt-client" 127.0.0.1:$SERVER_PORT fs > "$WORK/B2.log" 2>&1 || true
grep -q "peer fetch OK" "$WORK/B2.log" && ok "P2P 直连拉取成功（对等认证通过）" \
  || { grep -q "server miss" "$WORK/B2.log" && fail "P2P 直连失败（见 $WORK/B2.log）"; fail "未触发 peer 路径"; }

# ── 4. 冲突合并 Web GUI ──────────────────────────────
say "4) 冲突合并 Web GUI（conflicts-web）"
echo "local" > "$SB/plan.md"
echo "remote" > "$SB/.sync-conflict-aaaaaaaaaaaaaaa-plan.md-1770000000000"
export NSMT_SHARE_DIR="$SB"
"$BIN/nsmt-client" 127.0.0.1:$SERVER_PORT conflicts-web $GUI_PORT > "$WORK/gui.log" 2>&1 &
PIDS+=($!); sleep 1.5
CN=$(python3 -c "import urllib.parse;print(urllib.parse.quote('.sync-conflict-aaaaaaaaaaaaaaa-plan.md-1770000000000'))")
curl -sf "http://127.0.0.1:$GUI_PORT/api/conflicts" | jq -e '.conflicts | length >= 1' >/dev/null \
  && ok "GUI 列出冲突" || fail "GUI 列表失败"
curl -sf -X POST "http://127.0.0.1:$GUI_PORT/api/conflicts/$CN/resolve" \
  -H 'Content-Type: application/json' -d '{"choice":"custom","content":"merged"}' | jq -e '.ok' >/dev/null \
  && ok "custom 合并成功" || fail "合并失败"

# ── 5. ygg-admin 监督器（优雅重启 + kill -9 拉起）──────
say "5) ygg-admin 监督器（优雅重启 / 崩溃拉起）"
# ygg-admin 接管服务器生命周期：先停掉直启的服务器，避免端口冲突
kill -9 "$SERVER_PID" 2>/dev/null || true
sleep 1
"$BIN/nsmt-admin" --ygg "$BIN/nsmt-server" --control 127.0.0.1:$CONTROL_PORT \
  --bind 127.0.0.1:$ADMIN_PORT --token "$TOKEN" -- \
  127.0.0.1:$SERVER_PORT --control 127.0.0.1:$CONTROL_PORT > "$WORK/admin.log" 2>&1 &
PIDS+=($!); sleep 3
OLD=$(curl -sf "http://127.0.0.1:$ADMIN_PORT/api/process" | jq -r .pid)
[ -n "$OLD" ] && [ "$OLD" != "null" ] && ok "监督器已 spawn ygg (pid=$OLD)" || fail "监督器未 spawn (pid=$OLD)"
curl -sf -X POST -H "x-admin-token: $TOKEN" "http://127.0.0.1:$ADMIN_PORT/api/restart" >/dev/null
sleep 5
NEW=$(curl -sf "http://127.0.0.1:$ADMIN_PORT/api/process" | jq -r .pid)
[ "$OLD" != "$NEW" ] && ok "优雅重启成功 ($OLD→$NEW)" || fail "优雅重启失败"
kill -9 "$NEW"; sleep 3
RESTARTED=$(curl -sf "http://127.0.0.1:$ADMIN_PORT/api/process" | jq -r .pid)
[ "$RESTARTED" != "$NEW" ] && ok "kill -9 后自动拉起 ($NEW→$RESTARTED)" || fail "崩溃拉起失败"

# ── 6. M8 用户注册 / 升级 / 配额 ──────────────────────
say "6) 用户系统 + 会员升级（M8）"
BASE="http://127.0.0.1:$CONTROL_PORT"
TOK=$(curl -sf -X POST "$BASE/api/users/register" -H 'Content-Type: application/json' \
  -d '{"username":"alice","password":"secret123"}' | jq -r .token)
[ -n "$TOK" ] && ok "注册 alice 成功 (quota=50MB)" || fail "注册失败"
QUOTA=$(curl -sf -H "x-admin-token: $TOKEN" "$BASE/api/users" | jq -r '.users[0].quota_bytes')
[ "$QUOTA" = "52428800" ] && ok "free 配额 50MB" || fail "free 配额错误: $QUOTA"
curl -sf -X POST -H "x-admin-token: $TOKEN" "$BASE/api/users/alice/upgrade" \
  -H 'Content-Type: application/json' -d '{"plan":"pro"}' >/dev/null
QUOTA=$(curl -sf -H "x-admin-token: $TOKEN" "$BASE/api/users" | jq -r '.users[0].quota_bytes')
[ "$QUOTA" = "1073741824" ] && ok "升级 pro 配额 1GiB" || fail "pro 配额错误: $QUOTA"

# ── 7. 租户备份 / 恢复 ───────────────────────────────
say "7) 租户备份 / 恢复"
BK=$(curl -sf -H "x-admin-token: $TOKEN" "$BASE/api/backup?domain=$DOMAIN")
ARCHIVE=$(echo "$BK" | jq -r .archive)
[ -f "$ARCHIVE" ] && ok "备份完成: $ARCHIVE ($(echo "$BK" | jq -r .entries) entries)" || fail "备份失败"
rm -rf "$H/server/$DOMAIN"
curl -sf -X POST -H "x-admin-token: $TOKEN" "$BASE/api/restore" \
  -H 'Content-Type: application/json' -d "{\"domain\":\"$DOMAIN\",\"archive\":\"$ARCHIVE\"}" | jq -e '.ok' >/dev/null \
  && ok "恢复成功" || fail "恢复失败"
[ -f "$H/server/$DOMAIN/trees/latest.json" ] && ok "恢复后目录树存在" || fail "恢复后缺树"

printf '\n\033[1;32m═══════════ E2E 全部通过 ═══════════\033[0m\n'
[ -n "$KEEP" ] && echo "（保留测试目录: $WORK）" || true
