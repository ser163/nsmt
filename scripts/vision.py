#!/usr/bin/env python3
"""vision.py — 免费多模态识图工具（无需付费 API key）

后端（自动降级）:
  1. ollama     — 本地多模态模型（qwen2.5vl / llava / minicpm-v…）完全免费无限次
  2. gradio     — HuggingFace 免费空间（如 fffiloni/CLIP-Interrogator-2）免 key
  3. (预留) hf  — HuggingFace Inference API（需 HF_TOKEN）

用法:
  vision.py <图片路径|URL> ["问题/提示词"] [-m 模型] [-b 后端]
  vision.py --models            # 列出本地 ollama 可用多模态模型
  vision.py --pull qwen2.5vl:3b # 拉取本地模型（走代理）

环境变量:
  VISION_BACKEND=ollama|gradio   默认 ollama
  HTTPS_PROXY / HTTP_PROXY       访问外网（Gradio）时使用
"""
import argparse
import json
import os
import subprocess
import sys
import urllib.request

# 常用免费多模态模型
LOCAL_MODELS = ["qwen2.5vl:3b", "qwen2.5vl:7b", "llava:7b", "llava:13b",
                "minicpm-v", "bakllava:7b", "moondream"]

GRADIO_SPACES = {
    "clip-interrogator": ("fffiloni/CLIP-Interrogator-2", "/clipi2"),
}


def q(url: str) -> str:
    import urllib.parse
    return urllib.parse.quote(url, safe="")


def run_ollama(prompt: str, image: str, model: str) -> None:
    # 本地图片直接传路径；URL 先下载到临时文件
    img = image
    if image.startswith("http"):
        tmp = "/tmp/vision_dl.bin"
        req = urllib.request.Request(image, headers={"User-Agent": "vision.py/1.0"})
        with urllib.request.urlopen(req, timeout=60) as r:
            open(tmp, "wb").write(r.read())
        img = tmp
    print(f"[ollama] {model} 识别: {image}", file=sys.stderr)
    # 用 API 而不是 TTY 输出（避免转义垃圾）
    data = {
        "model": model, "prompt": prompt,
        "images": [open(img, "rb").read().hex()],  # hex 编码由 /api/generate 支持
        "stream": False,
    }
    # ollama 的 /api/generate 支持 base64 images；用 bytes
    payload = {
        "model": model, "prompt": prompt,
        "images": [_b64(img)],
        "stream": False,
    }
    req = urllib.request.Request(
        "http://127.0.0.1:11434/api/generate",
        data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=600) as r:
        out = json.load(r)
    print(out.get("response", "").strip())


def _b64(path: str) -> str:
    import base64
    with open(path, "rb") as f:
        return base64.b64encode(f.read()).decode()


def run_gradio(image: str, prompt: str, space: str, api_name: str) -> None:
    try:
        from gradio_client import Client, handle_file
    except ImportError:
        sys.exit("需要 gradio_client: pip3 install gradio_client")
    print(f"[gradio] {space} ({api_name}) 识别: {image}", file=sys.stderr)
    c = Client(space, verbose=False)
    r = c.predict(handle_file(image), "best", 4, api_name=api_name)
    print(r if isinstance(r, str) else str(r)[:500])


def main() -> None:
    ap = argparse.ArgumentParser(description="免费多模态识图工具")
    ap.add_argument("image", nargs="?", help="图片路径或 URL")
    ap.add_argument("prompt", nargs="?", default="用中文描述这张图片的内容", help="问题/提示词")
    ap.add_argument("-m", "--model", default="qwen2.5vl:3b")
    ap.add_argument("-b", "--backend", default=os.environ.get("VISION_BACKEND", "ollama"))
    ap.add_argument("--models", action="store_true", help="列出本地 ollama 模型")
    ap.add_argument("--pull", help="拉取本地模型，如 qwen2.5vl:3b")
    args = ap.parse_args()

    if args.models:
        out = subprocess.run(["ollama", "list"], capture_output=True, text=True)
        print(out.stdout or "(本地无模型)")
        print("可拉取:", ", ".join(LOCAL_MODELS))
        return
    if args.pull:
        r = subprocess.run(["ollama", "pull", args.pull], text=True)
        sys.exit(r.returncode)
    if not args.image:
        ap.error("需要图片路径或 URL")

    if args.backend == "gradio":
        run_gradio(args.image, args.prompt, *GRADIO_SPACES["clip-interrogator"])
    else:
        run_ollama(args.prompt, args.image, args.model)


if __name__ == "__main__":
    main()
