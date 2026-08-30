#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────
#  eyes-hands.sh — AI 的眼睛与绘画之手（Deepseek 的视觉/绘图）
#
#  🧿 眼睛 = 本地 qwen2.5vl（识图，免费无限次）
#  🎨 手   = Pollinations flux（生图，免费）
#
# 用法:
#   eyes-hands.sh see <图片> ["问题"]          # 识图（眼睛）
#   eyes-hands.sh draw "<提示词>" [-W w] [-H h] [-o out]   # 生图（手）
#   eyes-hands.sh cycle <图片>                 # 闭环：看图→描述→重绘→再识图
#   eyes-hands.sh chat [模型]                  # 交互聊天（可带图）
# ─────────────────────────────────────────────────────────────
set -uo pipefail
cd "$(dirname "$0")/.."

EYE_MODEL="${EYE_MODEL:-qwen2.5vl:3b}"
DRAW_MODEL="${DRAW_MODEL:-flux}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
GREEN=$'\033[32m'; CYAN=$'\033[36m'; YELLOW=$'\033[33m'; RESET=$'\033[0m'
step() { printf "\n${CYAN}━━ %s ━━${RESET}\n" "$*"; }

# 确保 ollama 在跑
ensure_ollama() { "$SCRIPT_DIR/ollama-tool.sh" status >/dev/null 2>&1 || "$SCRIPT_DIR/ollama-tool.sh" start >/dev/null 2>&1; }

# ── 眼睛：识图 ────────────────────────────────────────────
see() {
  local img="${1:-}" q="${2:-用中文详细描述这张图片的内容和风格}"
  [ -f "$img" ] || { echo "✘ 图片不存在: $img"; exit 1; }
  ensure_ollama
  step "🧿 识图（$EYE_MODEL）: $img"
  "$SCRIPT_DIR/ollama-tool.sh" chat "$EYE_MODEL" "$img" 2>&1 | sed -E 's/\x1b\[[0-9;]*m//g' | grep -v "识别中\|加载"
}

# ── 手：生图 ─────────────────────────────────────────────
draw() {
  local prompt="$1"; shift
  python3 "$SCRIPT_DIR/draw.py" "$prompt" "$@" 2>&1 | sed -E 's/\x1b\[[0-9;]*m//g' | grep -vE "^$"
}

# ── 闭环：看图 → 描述 → 重绘 → 再识图 ────────────────────
cycle() {
  local img="${1:-}"
  [ -f "$img" ] || { echo "✘ 图片不存在: $img"; exit 1; }
  local desc out re_out
  step "① 眼睛看图"
  desc="$(see "$img" "用英文给出这张图的详细视觉描述（颜色/构图/风格/元素），80词以内")"
  echo "$desc"
  out="/tmp/eyes_hand_cycle_$$.png"
  step "② 手重绘（基于描述）"
  draw "Repaint this scene: ${desc} , high quality digital art" -o "$out" -W 800 -H 600 -m "$DRAW_MODEL" -s 42 || { echo "✘ 生图失败"; exit 1; }
  echo "  生成: $out"
  step "③ 眼睛验证重绘结果"
  see "$out"
  echo; echo "✅ 闭环完成。重绘图: $out"
}

case "${1:-}" in
  see)   see "${2:-}" "${3:-}";;
  draw)  shift; draw "$@";;
  cycle) cycle "${2:-}";;
  chat)  ensure_ollama; exec ollama run "${2:-$EYE_MODEL}";;
  help|-h|--help)
    echo "用法:"
    echo "  eyes-hands.sh see <图片> [问题]      # 识图（眼睛）"
    echo "  eyes-hands.sh draw <提示词> [-W w] [-H h] [-o out] [-m 模型]   # 生图（手）"
    echo "  eyes-hands.sh cycle <图片>           # 闭环：看图→描述→重绘→再识图"
    echo "  eyes-hands.sh chat [模型]            # 交互聊天（可带图）"
    ;;
  *) echo "✘ 未知命令。eyes-hands.sh help 查看用法"; exit 1;;
esac
