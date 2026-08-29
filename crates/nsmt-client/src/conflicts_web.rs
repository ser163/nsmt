//! M9.2 冲突合并 Web GUI（对话式）：`yggd conflicts-web [--port 8088]`
//!
//! 内嵌 axum HTTP 服务（默认 127.0.0.1:8088），只服务本机：
//! - `GET  /`                        HTML 页（冲突列表 + 对话式合并）
//! - `GET  /api/conflicts`           列出冲突副本
//! - `GET  /api/conflicts/{name}`    冲突详情（主文件 vs 冲突副本内容）
//! - `POST /api/conflicts/{name}/resolve`  body `{"choice":"local|remote|custom","content":"…"}`
//!
//! 合并语义与 CLI `yggd merge` 一致：local = 保留冲突副本（本机修改），
//! remote = 保留主文件（远端版本），custom = 写入自定义内容。

use crate::fs;
use axum::{
    extract::Path,
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

/// 冲突条目（目录扫描 `.sync-conflict-*`）。
fn list_conflicts(dir: &std::path::Path) -> Vec<Value> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with(".sync-conflict-") {
                let meta = e.metadata().ok();
                out.push(json!({
                    "name": name,
                    "size": meta.map(|m| m.len()).unwrap_or(0),
                    "main": main_name_of(&name),
                }));
            }
        }
    }
    out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    out
}

/// 由冲突文件名推出主文件名（去掉 `.sync-conflict-<machine>-` 前缀和尾部 `-<ts>`）。
fn main_name_of(name: &str) -> String {
    let stripped = name.strip_prefix(".sync-conflict-").unwrap_or(name);
    let ts_idx = stripped.rfind('-').map(|i| i).unwrap_or(0);
    let machine_and_path = &stripped[..ts_idx];
    let dash = machine_and_path.find('-');
    match dash {
        Some(i) => machine_and_path[i + 1..].to_string(),
        None => machine_and_path.to_string(),
    }
}

fn conflict_path(dir: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    // 防目录穿越：只允许合法冲突文件名
    if name.starts_with('.') && !name.starts_with(".sync-conflict-") {
        return None;
    }
    let p = dir.join(name);
    if p.exists() && p.is_file() {
        Some(p)
    } else {
        None
    }
}

async fn index() -> axum::response::Html<&'static str> {
    axum::response::Html(INDEX_HTML)
}

async fn api_list() -> Json<Value> {
    let dir = fs::share_dir();
    Json(json!({ "share_dir": dir.display().to_string(), "conflicts": list_conflicts(&dir) }))
}

async fn api_detail(Path(name): Path<String>) -> Json<Value> {
    let dir = fs::share_dir();
    let Some(cp) = conflict_path(&dir, &name) else {
        return Json(json!({ "error": "conflict not found" }));
    };
    let main = dir.join(main_name_of(&name));
    let local = std::fs::read(&cp).unwrap_or_default();
    let remote = std::fs::read(&main).unwrap_or_default();
    Json(json!({
        "name": name,
        "main": main_name_of(&name),
        "local_bytes": local.len(),
        "remote_bytes": remote.len(),
        "local_text": String::from_utf8_lossy(&local).to_string(),
        "remote_text": String::from_utf8_lossy(&remote).to_string(),
    }))
}

#[derive(Deserialize)]
struct ResolveReq {
    choice: String,
    #[serde(default)]
    content: Option<String>,
}

async fn api_resolve(Path(name): Path<String>, Json(body): Json<ResolveReq>) -> Json<Value> {
    let dir = fs::share_dir();
    let Some(cp) = conflict_path(&dir, &name) else {
        return Json(json!({ "error": "conflict not found" }));
    };
    let main = dir.join(main_name_of(&name));
    match body.choice.as_str() {
        "local" => {
            let bytes = std::fs::read(&cp).unwrap_or_default();
            if std::fs::write(&main, &bytes).is_err() {
                return Json(json!({ "error": "write main failed" }));
            }
            let _ = std::fs::remove_file(&cp);
            Json(json!({ "ok": true, "action": "keep_local", "main": main.display().to_string() }))
        }
        "remote" => {
            let _ = std::fs::remove_file(&cp);
            Json(json!({ "ok": true, "action": "keep_remote" }))
        }
        "custom" => {
            let text = body.content.unwrap_or_default();
            if std::fs::write(&main, text.as_bytes()).is_err() {
                return Json(json!({ "error": "write main failed" }));
            }
            let _ = std::fs::remove_file(&cp);
            Json(json!({ "ok": true, "action": "custom_merged", "bytes": text.len() }))
        }
        other => Json(json!({ "error": format!("unknown choice: {other}") })),
    }
}

/// 启动冲突合并 Web 服务（阻塞运行）。
pub async fn serve(bind: std::net::SocketAddr) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/", get(index))
        .route("/api/conflicts", get(api_list))
        .route("/api/conflicts/{name}", get(api_detail))
        .route("/api/conflicts/{name}/resolve", axum::routing::post(api_resolve));
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!("conflict merge GUI on http://{bind}");
    axum::serve(listener, app).await?;
    Ok(())
}

const INDEX_HTML: &str = r#"<!DOCTYPE html>
<html lang="zh">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>NSMT 冲突合并</title>
<style>
  body { font-family: -apple-system, "PingFang SC", sans-serif; max-width: 960px; margin: 24px auto; padding: 0 16px; background: #f7f8fa; color: #222; }
  h1 { font-size: 22px; }
  .card { background: #fff; border: 1px solid #e3e6ea; border-radius: 8px; padding: 16px; margin-bottom: 16px; }
  .conf { display: flex; justify-content: space-between; align-items: center; padding: 8px 0; border-bottom: 1px dashed #eee; }
  .conf:last-child { border-bottom: none; }
  .grid { display: flex; gap: 12px; }
  .grid > div { flex: 1; }
  textarea { width: 100%; height: 220px; font-family: ui-monospace, Menlo, monospace; font-size: 13px; border: 1px solid #ccc; border-radius: 6px; padding: 8px; }
  button { padding: 8px 14px; border: none; border-radius: 6px; cursor: pointer; font-size: 14px; }
  .btn-local { background: #e8f5e9; color: #1b5e20; }
  .btn-remote { background: #fff3e0; color: #e65100; }
  .btn-custom { background: #e3f2fd; color: #0d47a1; }
  .tag { font-size: 12px; color: #666; background: #f0f0f0; padding: 2px 6px; border-radius: 4px; }
  .hidden { display: none; }
</style>
</head>
<body>
<h1>NSMT 冲突合并 <span class="tag" id="dir"></span></h1>
<div id="list" class="card"></div>
<div id="detail" class="card hidden">
  <h3 id="dname"></h3>
  <div class="grid">
    <div><b>本地修改版（冲突副本）</b><br><textarea id="local"></textarea></div>
    <div><b>远端版本（主文件）</b><br><textarea id="remote"></textarea></div>
  </div>
  <p><b>合并结果</b>：<textarea id="merged" placeholder="可在下方编辑自定义合并内容…"></textarea></p>
  <button class="btn-local" onclick="resolve('local')">保留本地</button>
  <button class="btn-remote" onclick="resolve('remote')">保留远端</button>
  <button class="btn-custom" onclick="resolve('custom')">写入合并内容</button>
  <span id="msg"></span>
</div>
<script>
async function load() {
  const r = await fetch('/api/conflicts');
  const d = await r.json();
  document.getElementById('dir').textContent = d.share_dir;
  const list = document.getElementById('list');
  list.innerHTML = '<h3>冲突副本 (' + d.conflicts.length + ')</h3>' + (d.conflicts.length === 0 ? '<p>无冲突，一切同步正常 🎉</p>' : '');
  for (const c of d.conflicts) {
    const div = document.createElement('div');
    div.className = 'conf';
    div.innerHTML = '<span><b>' + c.name + '</b> <span class="tag">' + c.size + ' B</span> → 主文件 ' + c.main + '</span>' +
      '<button onclick="openConf(\'' + c.name + '\')">对比合并</button>';
    list.appendChild(div);
  }
}
let current = null;
async function openConf(name) {
  current = name;
  const r = await fetch('/api/conflicts/' + encodeURIComponent(name));
  const d = await r.json();
  if (d.error) { alert(d.error); return; }
  document.getElementById('detail').classList.remove('hidden');
  document.getElementById('dname').textContent = name + '（主文件: ' + d.main + '）';
  document.getElementById('local').value = d.local_text;
  document.getElementById('remote').value = d.remote_text;
  document.getElementById('merged').value = '';
  document.getElementById('msg').textContent = '';
}
async function resolve(choice) {
  if (!current) return;
  const body = { choice };
  if (choice === 'custom') {
    body.content = document.getElementById('merged').value;
    if (!body.content.trim()) { alert('请输入合并内容'); return; }
  }
  const r = await fetch('/api/conflicts/' + encodeURIComponent(current) + '/resolve', {
    method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify(body)
  });
  const d = await r.json();
  document.getElementById('msg').textContent = d.ok ? '✅ ' + JSON.stringify(d) : '❌ ' + (d.error || '');
  if (d.ok) { document.getElementById('detail').classList.add('hidden'); current = null; load(); }
}
load();
</script>
</body>
</html>"#;
