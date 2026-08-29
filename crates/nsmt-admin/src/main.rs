//! NSMT ygg-admin：独立监督器 + Web 管理后台（M7）。
//!
//! 用法：
//!   ygg-admin [--ygg <path>] [--control <ip:port>] [--bind <ip:port>] [--token <t>] \
//!            [-- <ygg 参数…>]
//!
//! - 监督器：spawn ygg 子进程，监控健康，崩溃自动拉起（指数退避），
//!   `POST /api/admin/restart` 通过 ygg 控制 API 优雅重启（退出码 3 约定）。
//! - Web UI（默认 127.0.0.1:8090）：状态页（进程/在线/用量）、租户管理、日志、备份/恢复。
//! - 控制 API 客户端：聚合 ygg 控制 API（默认 127.0.0.1:8091）。

mod proc;

use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// 子进程管理状态。
struct YggProcess {
    pid: Option<u32>,
    started_at: Option<Instant>,
    exit_code: Option<i32>,
    restarts: u64,
    /// 距上次崩溃/重启的退避等待（毫秒）。
    backoff_ms: u64,
    /// 持有的子进程句柄（用于 try_wait 回收，避免僵尸）。
    child: Option<std::process::Child>,
}

#[derive(Clone)]
struct AdminState {
    ygg_path: String,
    control_base: String,
    admin_token: Option<String>,
    /// 交给子进程的额外参数（ygg 参数，如 --control）。
    ygg_args: Vec<String>,
    proc: Arc<RwLock<YggProcess>>,
    http: reqwest::Client,
}

/// 解析 `-- <args>` 之后的 ygg 参数。
fn split_ygg_args(args: &[String]) -> (Vec<String>, Vec<String>) {
    match args.iter().position(|a| a == "--") {
        Some(i) => (args[..i].to_vec(), args[i + 1..].to_vec()),
        None => (args.to_vec(), Vec::new()),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let raw: Vec<String> = std::env::args().skip(1).collect();
    let (own, ygg_args) = split_ygg_args(&raw);
    let mut ygg_path = std::env::var("YGG_BIN").unwrap_or_else(|_| "ygg".into());
    let mut control_addr = "127.0.0.1:8091".to_string();
    let mut bind = "127.0.0.1:8090".to_string();
    let mut admin_token = std::env::var("NSMT_ADMIN_TOKEN").ok().filter(|s| !s.is_empty());

    let mut i = 0;
    while i < own.len() {
        match own[i].as_str() {
            "--ygg" => { i += 1; ygg_path = own.get(i).cloned().unwrap_or(ygg_path); }
            "--control" => { i += 1; control_addr = own.get(i).cloned().unwrap_or(control_addr); }
            "--bind" => { i += 1; bind = own.get(i).cloned().unwrap_or(bind); }
            "--token" => { i += 1; admin_token = own.get(i).cloned(); }
            _ => {}
        }
        i += 1;
    }

    let state = AdminState {
        ygg_path,
        control_base: format!("http://{control_addr}"),
        admin_token,
        ygg_args,
        proc: Arc::new(RwLock::new(YggProcess {
            pid: None,
            started_at: None,
            exit_code: None,
            restarts: 0,
            backoff_ms: 0,
            child: None,
        })),
        http: reqwest::Client::new(),
    };

    // 启动监督循环（spawn 子进程 + 崩溃拉起）
    let sup_state = state.clone();
    tokio::spawn(async move {
        loop {
            // 若未运行 → 启动
            {
                let p = sup_state.proc.read().await;
                if p.pid.is_none() {
                    drop(p);
                    if let Err(e) = spawn_ygg(&sup_state).await {
                        tracing::error!("spawn ygg failed: {e}");
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        continue;
                    }
                }
            }
            // 等待子进程退出（try_wait 轮询，正常回收避免僵尸）
            let code = wait_child(&sup_state).await;
            let mut g = sup_state.proc.write().await;
            g.pid = None;
            g.child = None;
            g.restarts += 1;
            // 退出码 3 = 控制 API 请求的重启（优雅），不增加退避；其它退出按指数退避
            if code == Some(3) {
                g.backoff_ms = 0;
                tracing::info!("ygg restarted (graceful, code 3)");
            } else {
                g.backoff_ms = (g.backoff_ms * 2).clamp(0, 10_000);
                tracing::warn!("ygg exited (code={code:?}); restart #{}, backoff {}ms", g.restarts, g.backoff_ms);
            }
            drop(g);
            tokio::time::sleep(Duration::from_millis(backoff(&sup_state).await)).await;
        }
    });

    // Web UI + 控制 API 代理
    let listener = tokio::net::TcpListener::bind(bind.parse::<std::net::SocketAddr>()?).await?;
    tracing::info!("ygg-admin Web UI on http://{bind} (control {control_addr})");

    let app = Router::new()
        .route("/", get(index))
        .route("/api/process", get(api_process))
        .route("/api/restart", post(api_restart))
        .route("/api/status", get(proxy_status))
        .route("/api/tenants", get(proxy_tenants))
        .route("/api/online", get(proxy_online))
        .route("/api/locks", get(proxy_locks))
        .route("/api/logs", get(proxy_logs))
        .route("/api/users", get(proxy_users))
        .route("/api/backup", get(proxy_backup))
        .route("/api/restore", post(proxy_restore))
        .route("/api/tenants", post(proxy_add_tenant))
        .with_state(state);

    axum::serve(listener, app).await?;
    Ok(())
}

async fn backoff(state: &AdminState) -> u64 {
    let p = state.proc.read().await;
    p.backoff_ms.max(500)
}

async fn spawn_ygg(state: &AdminState) -> anyhow::Result<()> {
    let mut cmd = std::process::Command::new(&state.ygg_path);
    cmd.args(&state.ygg_args)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let child = cmd.spawn().map_err(|e| anyhow::anyhow!("spawn {}: {e}", state.ygg_path))?;
    let pid = child.id();
    let mut g = state.proc.write().await;
    g.pid = Some(pid);
    g.started_at = Some(Instant::now());
    g.exit_code = None;
    g.backoff_ms = 0;
    g.child = Some(child);
    tracing::info!("ygg spawned pid={pid}");
    Ok(())
}

/// 轮询子进程是否存活（非阻塞）：`try_wait` 正常回收（避免僵尸）。
async fn wait_child(state: &AdminState) -> Option<i32> {
    loop {
        {
            let mut g = state.proc.write().await;
            if let Some(ch) = g.child.as_mut() {
                match ch.try_wait() {
                    Ok(Some(status)) => {
                        let code = status.code();
                        g.child = None;
                        g.pid = None;
                        return code;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!("try_wait error: {e}; treat as exited");
                        g.child = None;
                        g.pid = None;
                        return None;
                    }
                }
            } else {
                return None;
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// 终止子进程（SIGTERM → 等待 → SIGKILL）。
fn terminate(pid: u32) {
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill").arg("-TERM").arg(pid.to_string()).status();
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(100));
            if std::process::Command::new("kill").arg("-0").arg(pid.to_string()).status().map(|s| !s.success()).unwrap_or(true) {
                return;
            }
        }
        let _ = std::process::Command::new("kill").arg("-KILL").arg(pid.to_string()).status();
    }
}

// ── Web UI ──

async fn index() -> axum::response::Html<&'static str> {
    axum::response::Html(INDEX_HTML)
}

async fn api_process(State(s): State<AdminState>) -> Json<Value> {
    let p = s.proc.read().await;
    let (pid, uptime, restarts) = (p.pid, p.started_at, p.restarts);
    drop(p);
    let mut cpu = 0.0f32;
    let mut mem_mb = 0.0f32;
    if let Some(pid) = pid {
        if let Ok((c, m)) = proc::ps_usage(pid) {
            cpu = c;
            mem_mb = m;
        }
    }
    Json(json!({
        "pid": pid,
        "running": pid.is_some(),
        "uptime_s": uptime.map(|t| t.elapsed().as_secs()).unwrap_or(0),
        "restarts": restarts,
        "cpu_pct": cpu,
        "mem_mb": mem_mb,
        "control": s.control_base,
    }))
}

async fn api_restart(State(s): State<AdminState>) -> Json<Value> {
    // 先尝试 ygg 控制 API 优雅重启；失败则直接 kill（监督器拉起）
    let p = s.proc.read().await;
    if let Some(pid) = p.pid {
        drop(p);
        let url = format!("{}/api/admin/restart", s.control_base);
        let mut req = s.http.post(&url);
        if let Some(t) = &s.admin_token {
            req = req.header("x-admin-token", t);
        }
        let resp = req.send().await;
        match resp {
            Ok(r) => {
                let v: Value = r.json().await.unwrap_or(json!({}));
                tracing::info!("ygg restart requested via control API");
                // 控制 API 返回后 ygg 会以码 3 退出，监督器自动拉起
                return Json(json!({"ok": true, "via": "control", "pid": pid, "detail": v}));
            }
            Err(e) => {
                tracing::warn!("control API restart failed ({e}); hard-kill pid {pid}");
                terminate(pid);
                return Json(json!({"ok": true, "via": "kill", "pid": pid}));
            }
        }
    }
    Json(json!({"error": "ygg not running"}))
}

fn authorized(s: &AdminState, headers: &axum::http::HeaderMap) -> bool {
    match &s.admin_token {
        Some(t) => headers.get("x-admin-token").and_then(|v| v.to_str().ok()) == Some(t.as_str()),
        None => true,
    }
}

/// 构造带 token 的 GET 请求（代理到 ygg 控制 API 时透传管理凭证）。
fn authed_get(s: &AdminState, url: &str) -> reqwest::RequestBuilder {
    let mut req = s.http.get(url);
    if let Some(t) = &s.admin_token {
        req = req.header("x-admin-token", t);
    }
    req
}

async fn proxy_tenants(
    State(s): State<AdminState>,
    headers: axum::http::HeaderMap,
) -> Json<Value> {
    if !authorized(&s, &headers) {
        return Json(json!({"error": "unauthorized"}));
    }
    match authed_get(&s, &format!("{}/api/tenants", s.control_base)).send().await {
        Ok(r) => Json(r.json().await.unwrap_or(json!({}))),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

async fn proxy_add_tenant(
    State(s): State<AdminState>,
    headers: axum::http::HeaderMap,
    Json(b): Json<Value>,
) -> Json<Value> {
    if !authorized(&s, &headers) {
        return Json(json!({"error": "unauthorized"}));
    }
    let mut req = s.http.post(format!("{}/api/tenants", s.control_base)).json(&b);
    if let Some(t) = &s.admin_token {
        req = req.header("x-admin-token", t);
    }
    match req.send().await {
        Ok(r) => Json(r.json().await.unwrap_or(json!({}))),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

async fn proxy_online(State(s): State<AdminState>, headers: axum::http::HeaderMap) -> Json<Value> {
    if !authorized(&s, &headers) {
        return Json(json!({"error": "unauthorized"}));
    }
    match authed_get(&s, &format!("{}/api/online", s.control_base)).send().await {
        Ok(r) => Json(r.json().await.unwrap_or(json!({}))),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

async fn proxy_locks(State(s): State<AdminState>, headers: axum::http::HeaderMap) -> Json<Value> {
    if !authorized(&s, &headers) {
        return Json(json!({"error": "unauthorized"}));
    }
    match authed_get(&s, &format!("{}/api/locks", s.control_base)).send().await {
        Ok(r) => Json(r.json().await.unwrap_or(json!({}))),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

async fn proxy_status(State(s): State<AdminState>, headers: axum::http::HeaderMap) -> Json<Value> {
    if !authorized(&s, &headers) {
        return Json(json!({"error": "unauthorized"}));
    }
    match authed_get(&s, &format!("{}/api/status", s.control_base)).send().await {
        Ok(r) => Json(r.json().await.unwrap_or(json!({}))),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

#[derive(Deserialize)]
struct LogQuery { lines: Option<usize>, level: Option<String> }

async fn proxy_logs(
    State(s): State<AdminState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<LogQuery>,
) -> Json<Value> {
    if !authorized(&s, &headers) {
        return Json(json!({"error": "unauthorized"}));
    }
    let lines = q.lines.unwrap_or(200);
    let url = match &q.level {
        Some(l) => format!("{}/api/logs?lines={}&filter={}", s.control_base, lines, l),
        None => format!("{}/api/logs?lines={}", s.control_base, lines),
    };
    match authed_get(&s, &url).send().await {
        Ok(r) => Json(r.json().await.unwrap_or(json!({}))),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

async fn proxy_users(State(s): State<AdminState>, headers: axum::http::HeaderMap) -> Json<Value> {
    if !authorized(&s, &headers) {
        return Json(json!({"error": "unauthorized"}));
    }
    match authed_get(&s, &format!("{}/api/users", s.control_base)).send().await {
        Ok(r) => Json(r.json().await.unwrap_or(json!({}))),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

#[derive(Deserialize)]
struct BackupQuery { domain: Option<String> }

async fn proxy_backup(
    State(s): State<AdminState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<BackupQuery>,
) -> Json<Value> {
    if !authorized(&s, &headers) {
        return Json(json!({"error": "unauthorized"}));
    }
    let url = match &q.domain {
        Some(d) => format!("{}/api/backup?domain={}", s.control_base, d),
        None => format!("{}/api/backup", s.control_base),
    };
    match authed_get(&s, &url).send().await {
        Ok(r) => Json(r.json().await.unwrap_or(json!({}))),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

async fn proxy_restore(
    State(s): State<AdminState>,
    headers: axum::http::HeaderMap,
    Json(b): Json<Value>,
) -> Json<Value> {
    if !authorized(&s, &headers) {
        return Json(json!({"error": "unauthorized"}));
    }
    let mut req = s.http.post(format!("{}/api/restore", s.control_base)).json(&b);
    if let Some(t) = &s.admin_token {
        req = req.header("x-admin-token", t);
    }
    match req.send().await {
        Ok(r) => Json(r.json().await.unwrap_or(json!({}))),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

const INDEX_HTML: &str = r#"<!DOCTYPE html>
<html lang="zh">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>NSMT ygg-admin</title>
<style>
  body { font-family: -apple-system, "PingFang SC", sans-serif; max-width: 1100px; margin: 24px auto; padding: 0 16px; background: #f7f8fa; color: #222; }
  h1 { font-size: 22px; }
  .card { background: #fff; border: 1px solid #e3e6ea; border-radius: 8px; padding: 16px; margin-bottom: 16px; }
  .row { display: flex; gap: 24px; flex-wrap: wrap; }
  .metric { font-size: 28px; font-weight: 600; }
  .metric-label { font-size: 12px; color: #888; }
  .bar { background: #eef0f3; border-radius: 6px; height: 10px; overflow: hidden; margin: 6px 0; }
  .bar > div { background: #4c9aff; height: 100%; }
  table { width: 100%; border-collapse: collapse; font-size: 14px; }
  th, td { text-align: left; padding: 6px 8px; border-bottom: 1px solid #eee; }
  button { padding: 6px 12px; border: none; border-radius: 6px; cursor: pointer; font-size: 13px; background: #e3f2fd; color: #0d47a1; }
  button.danger { background: #ffebee; color: #b71c1c; }
  pre.log { max-height: 360px; overflow: auto; font-size: 12px; background: #0d1117; color: #c9d1d9; padding: 12px; border-radius: 8px; }
  .hidden { display: none; }
  .tag { font-size: 12px; padding: 2px 8px; border-radius: 10px; }
  .tag.free { background: #eceff1; color: #455a64; }
  .tag.pro { background: #fff8e1; color: #f57f17; }
</style>
</head>
<body>
<h1>NSMT ygg-admin <span id="ctrl" class="tag" style="background:#e8f5e9;color:#1b5e20"></span></h1>

<div id="proc" class="card row"></div>
<div class="row">
  <div class="card" style="flex:1"><h3>在线机器</h3><div id="online"></div></div>
  <div class="card" style="flex:1"><h3>租户</h3><div id="tenants"></div></div>
</div>
<div class="row">
  <div class="card" style="flex:1"><h3>用户与配额</h3><div id="users"></div></div>
  <div class="card" style="flex:1"><h3>备份 / 恢复</h3><div id="backup">
    <input id="bkdomain" placeholder="domain（如 ser163）" style="padding:6px;width:60%">
    <button onclick="backup()">备份</button><br><br>
    <input id="rsdomain" placeholder="domain" style="padding:6px;width:25%">
    <input id="rsarchive" placeholder="归档路径" style="padding:6px;width:45%">
    <button class="danger" onclick="restore()">恢复</button>
    <div id="bkmsg" style="margin-top:8px;font-size:13px"></div>
  </div></div>
</div>
<div class="card"><h3>日志 <button onclick="loadLogs()">刷新</button> <input id="loglines" value="200" style="width:60px;padding:4px"></h3><pre class="log" id="logs"></pre></div>

<script>
const HEADERS = {};
async function j(url, opt) {
  const r = await fetch(url, Object.assign({headers: HEADERS}, opt));
  return r.json();
}
async function loadProcess() {
  const d = await j('/api/process');
  document.getElementById('proc').innerHTML =
    '<div><div class="metric-label">进程状态</div><div class="metric">' + (d.running ? '🟢 运行中' : '🔴 已停止') + '</div><div>PID ' + (d.pid||'-') + '</div></div>' +
    '<div><div class="metric-label">运行时长</div><div class="metric">' + (d.uptime_s||0) + 's</div></div>' +
    '<div><div class="metric-label">CPU</div><div class="metric">' + (d.cpu_pct||0) + '%</div></div>' +
    '<div><div class="metric-label">内存</div><div class="metric">' + (d.mem_mb||0) + ' MB</div></div>' +
    '<div><div class="metric-label">重启次数</div><div class="metric">' + (d.restarts||0) + '</div></div>' +
    '<div style="align-self:center"><button class="danger" onclick="restart()">重启 ygg</button></div>';
  document.getElementById('ctrl').textContent = 'control: ' + (d.control||'');
}
async function restart() {
  if (!confirm('确认重启 ygg 服务器？')) return;
  const d = await j('/api/restart', {method:'POST'});
  alert(JSON.stringify(d));
  setTimeout(loadAll, 1500);
}
async function loadOnline() {
  const d = await j('/api/online');
  const online = d.online || {};
  let html = '<table><tr><th>域</th><th>机器</th><th>地址</th><th>Agent</th></tr>';
  for (const [domain, machines] of Object.entries(online)) {
    for (const m of machines) {
      html += '<tr><td>' + domain + '</td><td>' + m.machine_id + '</td><td>' + (m.peer_addr || m.addr || '-') + '</td><td>' + (m.agents||[]).join(',') + '</td></tr>';
    }
  }
  html += '</table>';
  document.getElementById('online').innerHTML = html || '（无在线机器）';
}
async function loadTenants() {
  const d = await j('/api/tenants');
  const ts = d.tenants || [];
  let html = '<table><tr><th>域</th><th>机器数</th><th>用量</th></tr>';
  for (const t of ts) {
    html += '<tr><td>' + t.domain + '</td><td>' + (t.machines||0) + '</td><td>' + fmtBytes(t.usage_bytes||0) + '</td></tr>';
  }
  html += '</table>';
  document.getElementById('tenants').innerHTML = html || '（无租户）';
}
async function loadUsers() {
  const d = await j('/api/users');
  const us = d.users || [];
  let html = '<table><tr><th>用户名</th><th>套餐</th><th>用量</th><th>配额</th><th></th></tr>';
  for (const u of us) {
    const pct = Math.min(100, Math.round(100 * (u.usage_bytes||0) / (u.quota_bytes||1)));
    html += '<tr><td>' + u.username + '</td>' +
      '<td><span class="tag ' + u.plan + '">' + u.plan + '</span></td>' +
      '<td>' + fmtBytes(u.usage_bytes||0) + '<div class="bar"><div style="width:' + pct + '%"></div></div></td>' +
      '<td>' + fmtBytes(u.quota_bytes||0) + '</td>' +
      '<td>' + (u.plan === 'pro' ? '' : '<button onclick="upgrade(\'' + u.username + '\')">升级 Pro</button>') + '</td></tr>';
  }
  html += '</table>';
  document.getElementById('users').innerHTML = html || '（无用户，设置 NSMT_DB_URL 启用用户系统）';
}
async function upgrade(username) {
  if (!confirm('确认将 ' + username + ' 升级为 Pro（1 GiB）？')) return;
  const d = await j('/api/users/' + username + '/upgrade', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify({plan:'pro'})});
  alert(JSON.stringify(d));
  loadUsers();
}
async function loadLogs() {
  const lines = document.getElementById('loglines').value || 200;
  const d = await j('/api/logs?lines=' + lines);
  document.getElementById('logs').textContent = d.content || (d.error || '（无日志）');
}
async function backup() {
  const domain = document.getElementById('bkdomain').value.trim();
  if (!domain) { alert('请输入域名'); return; }
  const d = await j('/api/backup?domain=' + encodeURIComponent(domain));
  document.getElementById('bkmsg').textContent = JSON.stringify(d);
}
async function restore() {
  const domain = document.getElementById('rsdomain').value.trim();
  const archive = document.getElementById('rsarchive').value.trim();
  if (!domain || !archive) { alert('请填写域名与归档路径'); return; }
  if (!confirm('恢复将覆盖该租户数据，确认？')) return;
  const d = await j('/api/restore', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify({domain, archive})});
  document.getElementById('bkmsg').textContent = JSON.stringify(d);
}
function fmtBytes(n) {
  if (n < 1024) return n + ' B';
  if (n < 1048576) return (n/1024).toFixed(1) + ' KB';
  return (n/1048576).toFixed(1) + ' MB';
}
async function loadAll() { loadProcess(); loadOnline(); loadTenants(); loadUsers(); loadLogs(); }
setInterval(loadAll, 5000);
loadAll();
</script>
</body>
</html>"#;
