#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────
#  Ollama 人性化管理工具（macOS / Linux）
#  启动 / 停止 / 状态 / 开机自启 / 模型管理 / 聊天识图 / 磁盘
# ─────────────────────────────────────────────────────────────
set -uo pipefail

PORT="${OLLAMA_PORT:-11434}"
PID_FILE="${HOME}/.ollama/nsmt-serve.pid"
BOLD=$'\033[1m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'; RED=$'\033[31m'; CYAN=$'\033[36m'; DIM=$'\033[2m'; RESET=$'\033[0m'
ok()   { printf "${GREEN}  ✔ %s${RESET}\n" "$*"; }
warn() { printf "${YELLOW}  ⚠ %s${RESET}\n" "$*"; }
err()  { printf "${RED}  ✘ %s${RESET}\n" "$*"; }
info() { printf "${CYAN}  %s${RESET}\n" "$*"; }

is_running() { nc -z 127.0.0.1 "$PORT" >/dev/null 2>&1; }

service_pid() {
  # 优先 pidfile，否则 ps 找
  if [ -f "$PID_FILE" ] && kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
    cat "$PID_FILE"; return
  fi
  pgrep -f "ollama serve" | head -1
}

# ── 状态总览 ────────────────────────────────────────────────
status() {
  echo; echo "${BOLD}🦙 Ollama 状态${RESET}"
  if is_running; then
    PID="$(service_pid)"; ok "服务运行中  (pid=${PID:-?}, 端口 ${PORT})"
    VER="$(ollama --version 2>/dev/null | head -1 || echo '?')"; info "版本: ${VER}"
  else
    err "服务未运行"
  fi
  echo "  模型:"
  ollama list 2>/dev/null | awk 'NR==1{print "    "$0; next} {printf "    %s\n", $0}' || true
  echo "  磁盘:"
  if [ -d "$HOME/.ollama/models" ]; then
    du -sh "$HOME/.ollama/models" 2>/dev/null | awk '{printf "    %s (模型库 %s)\n", $1, "'"$HOME"'/.ollama/models"}'
  fi
  echo
  # brew 托管状态
  if command -v brew >/dev/null 2>&1; then
    BS="$(brew services list 2>/dev/null | awk '$1=="ollama"{print $2}')"
    [ "$BS" = "started" ] && info "开机自启: brew 已托管（开机自动运行）" || warn "开机自启: 未托管（brew）—— 可用 autostart on 开启"
  fi
  echo
}

# ── 启动 ────────────────────────────────────────────────────
start() {
  if is_running; then ok "已在运行（端口 ${PORT}），无需重复启动"; return; fi
  command -v ollama >/dev/null 2>&1 || { err "未安装 ollama（brew install ollama）"; return 1; }
  echo "  启动 ollama serve …"
  nohup ollama serve >/dev/null 2>&1 &
  echo $! > "$PID_FILE"
  sleep 2
  if is_running; then ok "启动成功（pid $(service_pid)）"; else err "启动失败，看日志：~/Library/Logs 或 ollama serve"; return 1; fi
}

# ── 停止 ────────────────────────────────────────────────────
stop() {
  if ! is_running; then warn "本来就没在运行"; return; fi
  PID="$(service_pid)"
  echo "  停止 ollama (pid=${PID:-?}) …"
  kill "${PID:-}" 2>/dev/null
  # 兜底：nohup 起的孤儿进程
  pkill -f "ollama serve" 2>/dev/null || true
  sleep 1
  is_running && err "停止失败" || ok "已停止"
}

# ── 开机自启（brew 托管）────────────────────────────────────
autostart() {
  case "${1:-}" in
    on|enable|start)
      command -v brew >/dev/null 2>&1 || { err "需要 brew"; return 1; }
      # 先停掉手动进程，交给 brew 托管
      if [ -f "$PID_FILE" ]; then kill "$(cat "$PID_FILE")" 2>/dev/null; rm -f "$PID_FILE"; fi
      pkill -f "ollama serve" 2>/dev/null || true
      sleep 1
      brew services start ollama >/dev/null 2>&1
      sleep 2
      is_running && ok "已注册开机自启，服务运行中" || warn "brew 已登记，但服务暂未就绪（稍等或 brew services list）"
      ;;
    off|disable|stop)
      brew services stop ollama >/dev/null 2>&1
      ok "已关闭开机自启（可随时 start 手动启动）"
      ;;
    *)
      echo "用法: ollama-tool autostart on|off"
      ;;
  esac
}

# ── 模型列表 / 拉取 / 删除 ─────────────────────────────────
mlist() { echo "  当前模型:"; ollama list 2>&1; echo; }
mpull() {
  local m="${1:-}"; [ -z "$m" ] && { err "用法: ollama-tool pull <模型>，如 qwen2.5vl:3b"; return 1; }
  info "拉取 ${m}（大文件走代理可在命令前 export HTTPS_PROXY=…）…"
  ollama pull "$m" && ok "拉取完成"
}
mrm() {
  local m="${1:-}"; [ -z "$m" ] && { err "用法: ollama-tool rm <模型>"; return 1; }
  ollama rm "$m" >/dev/null 2>&1 && ok "已删除 ${m}" || err "删除失败（不存在？）"
}

# ── 聊天 / 识图 ─────────────────────────────────────────────
chat() {
  is_running || start
  local model="${1:-qwen2.5vl:3b}"
  local img="${2:-}"
  if [ -n "$img" ]; then
    [ -f "$img" ] || { err "图片不存在: $img"; return 1; }
    echo; echo "  📷 已加载图片: $img"
    echo "  识别中（${model}）…"; echo
    # 干净输出：走 ollama REST API，避免 TTY 转义垃圾
    python3 - "$model" "$img" <<'PYEOF'
import base64, json, sys, urllib.request
model, img = sys.argv[1], sys.argv[2]
with open(img, "rb") as f:
    b64 = base64.b64encode(f.read()).decode()
payload = {"model": model, "prompt": "用中文详细描述这张图片的内容和风格", "images": [b64], "stream": False}
req = urllib.request.Request("http://127.0.0.1:11434/api/generate",
                             data=json.dumps(payload).encode(),
                             headers={"Content-Type": "application/json"})
with urllib.request.urlopen(req, timeout=600) as r:
    print(json.load(r).get("response", "").strip())
PYEOF
    return
  fi
  info "进入聊天（${model}），输入 exit 退出"
  ollama run "$model"
}

# ── 磁盘 ───────────────────────────────────────────────────
disk() {
  du -sh "$HOME/.ollama/models" 2>/dev/null | awk '{printf "  模型库占用: %s (%s)\n", $1, "'"$HOME"'/.ollama/models"}'
  du -sh "$HOME/.ollama" 2>/dev/null | awk '{printf "  ollama 全部: %s\n", $1}'
}

# ── 交互菜单 ───────────────────────────────────────────────
menu() {
  while true; do
    echo
    echo "${BOLD}🦙 Ollama 管理${RESET}"
    echo "  ${CYAN}1${RESET}) 状态总览    ${CYAN}2${RESET}) 启动         ${CYAN}3${RESET}) 停止"
    echo "  ${CYAN}4${RESET}) 开机自启 on  ${CYAN}5${RESET}) 开机自启 off ${CYAN}6${RESET}) 模型列表"
    echo "  ${CYAN}7${RESET}) 拉取模型     ${CYAN}8${RESET}) 删除模型     ${CYAN}9${RESET}) 聊天/识图"
    echo "  ${CYAN}0${RESET}) 磁盘占用     ${CYAN}q${RESET}) 退出"
    printf "  选择: "; read -r c
    case "$c" in
      1) status;; 2) start;; 3) stop;;
      4) autostart on;; 5) autostart off;; 6) mlist;;
      7) printf "  模型名: "; read -r m; mpull "$m";;
      8) printf "  模型名: "; read -r m; mrm "$m";;
      9) printf "  模型(默认qwen2.5vl:3b): "; read -r m; chat "${m:-qwen2.5vl:3b}";;
      0) disk;; q|Q) echo "  再见 👋"; exit 0;;
      *) warn "无效选择";;
    esac
  done
}

# ── 主入口 ─────────────────────────────────────────────────
case "${1:-}" in
  status|start|stop|list|disk) "$@";;
  pull|rm) "$@" "${2:-}";;
  chat) chat "${2:-qwen2.5vl:3b}" "${3:-}";;
  autostart) autostart "${2:-}";;
  "") menu;;
  help|-h|--help) sed -n '1,10p' "$0" | sed 's/^# \{0,1\}//'; echo "用法: ollama-tool [status|start|stop|list|pull <m>|rm <m>|chat [m] [img]|autostart on|off|disk]";;
  *) warn "未知命令: $1（ollama-tool help 查看用法）";;
esac
