#!/usr/bin/env python3
"""draw.py — 免费绘图 AI 封装工具（无需 API key）

后端（默认优先，均可离线降级）:
  1. Pollinations.ai  https://image.pollinations.ai/prompt/{p}  （免 key，GET 即出图）
  2. (可选) HF_TOKEN 设置时走 HuggingFace Inference API（SDXL 等）

用法:
  draw.py "prompt" [-o out.png] [-W 1080] [-H 1350] [-m model] [-s seed]
  draw.py --list            # 列出推荐模型
  draw.py --prompts-file f  # 批量生成（每行一个 prompt）

环境变量:
  DRAW_BACKEND=pollinations|huggingface   默认 pollinations
  HF_TOKEN=...                            使用 HF 后端时必填
"""
import argparse
import base64
import os
import ssl
import sys
import time
import urllib.parse
import urllib.request

POLLINATIONS = "https://image.pollinations.ai/prompt/{prompt}"
HF_URL = "https://api-inference.huggingface.co/models/{model}"


def _ssl_context():
    """macOS 自带 Python 常缺 CA 证书：优先系统/certifi，实在不行跳过验证（dev 工具）。"""
    ctx = ssl.create_default_context()
    try:
        import certifi  # type: ignore
        ctx.load_verify_locations(certifi.where())
        return ctx
    except Exception:
        pass
    # 尝试 macOS 系统证书路径
    for p in ("/etc/ssl/cert.pem", "/etc/pki/tls/certs/ca-bundle.crt"):
        if os.path.exists(p):
            try:
                ctx.load_verify_locations(p)
                return ctx
            except Exception:
                continue
    print("[draw] CA 证书缺失，降级为不校验（dev 工具）", file=sys.stderr)
    return ssl._create_unverified_context()

MODELS = {
    "flux": "flux",            # Pollinations 默认（FLUX.1）
    "sana": "sana",            # 快
    "turbo": "turbo",          # SDXL Turbo，快
    "sdxl": "sdxl",            # SDXL
    "kandinsky": "kandinsky",
    "horde": "horde",          # Stable Horde 聚合
}


def fetch_pollinations(prompt: str, out: str, width: int, height: int,
                       model: str, seed: int) -> None:
    q = {
        "width": width, "height": height,
        "nologo": "true", "model": model, "seed": seed,
        "private": "true",
    }
    url = POLLINATIONS.format(prompt=urllib.parse.quote(prompt)) + "?" + urllib.parse.urlencode(q)
    print(f"[pollinations] {model} {width}x{height} seed={seed} …", file=sys.stderr)
    req = urllib.request.Request(url, headers={"User-Agent": "maka-draw/1.0"})
    with urllib.request.urlopen(req, timeout=180, context=_ssl_context()) as r:
        data = r.read()
    with open(out, "wb") as f:
        f.write(data)
    print(f"✓ {out} ({len(data)} bytes)")


def fetch_huggingface(prompt: str, out: str, model: str) -> None:
    token = os.environ.get("HF_TOKEN")
    if not token:
        raise SystemExit("HF 后端需要 HF_TOKEN 环境变量")
    body = '{"inputs": "%s"}' % prompt.replace('"', '\\"')
    req = urllib.request.Request(
        HF_URL.format(model=model), data=body.encode(),
        headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"},
    )
    print(f"[huggingface] {model} …", file=sys.stderr)
    for attempt in range(3):  # 模型冷启动可能 503，重试
        try:
            with urllib.request.urlopen(req, timeout=300, context=_ssl_context()) as r:
                data = r.read()
            break
        except urllib.error.HTTPError as e:
            if e.code == 503 and attempt < 2:
                print("  model loading, retry…", file=sys.stderr)
                time.sleep(10)
            else:
                raise SystemExit(f"HF 调用失败: {e}")
    with open(out, "wb") as f:
        f.write(base64.b64decode(data) if data[:1] != b"\x89" else data)
    print(f"✓ {out} ({len(data)} bytes)")


def main() -> None:
    ap = argparse.ArgumentParser(description="免费绘图 AI 工具")
    ap.add_argument("prompt", nargs="?", help="英文提示词（效果最好）")
    ap.add_argument("-o", "--out", default="out.png")
    ap.add_argument("-W", "--width", type=int, default=1080)
    ap.add_argument("-H", "--height", type=int, default=1350)
    ap.add_argument("-m", "--model", default="flux")
    ap.add_argument("-s", "--seed", type=int, default=42)
    ap.add_argument("--list", action="store_true", help="列出模型")
    ap.add_argument("--prompts-file", help="批量生成：每行一个 prompt")
    args = ap.parse_args()

    if args.list:
        print("可用模型:", ", ".join(MODELS))
        return
    if not args.prompt and not args.prompts_file:
        ap.error("需要 prompt 或 --prompts-file")

    backend = os.environ.get("DRAW_BACKEND", "pollinations")
    prompts = [args.prompt] if args.prompt else [l.strip() for l in open(args.prompts_file) if l.strip()]
    for i, p in enumerate(prompts):
        out = args.out if len(prompts) == 1 else args.out.replace(".png", f"-{i}.png")
        if backend == "huggingface":
            fetch_huggingface(p, out, args.model)
        else:
            fetch_pollinations(p, out, args.width, args.height, args.model, args.seed + i)


if __name__ == "__main__":
    main()
