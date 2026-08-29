//! ygg 控制 API（Web 后台数据面，M6.1）。
//!
//! `ygg --control 127.0.0.1:8091` 启用。仅供本机/内网访问；生产建议加 token。
//! 端点：
//!   GET  /api/status     健康/uptime/pid/配额/用量
//!   GET  /api/tenants    租户列表 + 用量；POST 添加租户（域+公钥）
//!   GET  /api/online     在线机器/agent
//!   GET  /api/locks      锁状态
//!   GET  /api/logs?lines=N  日志 tail（NSMT_LOG_FILE）

use crate::state::ServerState;
use std::sync::Arc as StdArc;
use crate::tenants::TenantStore;
use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone)]
struct AdminState {
    state: StdArc<ServerState>,
    tenants: Arc<TenantStore>,
    log_file: std::path::PathBuf,
    started_at: std::time::Instant,
    admin_token: Option<String>,
}

/// 启动控制 API 服务器（阻塞运行，放到独立 task）。
pub async fn spawn(
    state: StdArc<ServerState>,
    tenants: Arc<TenantStore>,
    bind: std::net::SocketAddr,
) -> anyhow::Result<()> {
    let log_file = std::env::var("NSMT_LOG_FILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(|h| std::path::PathBuf::from(h).join(".nsmt/logs/ygg.log"))
                .unwrap_or_else(|_| std::path::PathBuf::from(".nsmt/logs/ygg.log"))
        });
    let admin_token = std::env::var("NSMT_ADMIN_TOKEN").ok().filter(|s| !s.is_empty());

    let app_state = AdminState {
        state,
        tenants,
        log_file,
        started_at: std::time::Instant::now(),
        admin_token,
    };

    let app = Router::new()
        .route("/api/status", get(status))
        .route("/api/tenants", get(list_tenants).post(add_tenant))
        .route("/api/online", get(list_online))
        .route("/api/locks", get(list_locks))
        .route("/api/logs", get(logs))
        .route("/api/users/register", post(register_user))
        .route("/api/users/login", post(login_user))
        .route("/api/tenants/key", post(set_tenant_key))
        // M8：会员/配额 UI
        .route("/api/users", get(list_users))
        .route("/api/users/{username}/upgrade", post(upgrade_user))
        // M7：重启信号
        .route("/api/admin/restart", post(admin_restart))
        // 待办池：租户备份/恢复
        .route("/api/backup", get(backup_tenant))
        .route("/api/restore", post(restore_tenant))
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!("admin control API on http://{bind}");
    axum::serve(listener, app).await?;
    Ok(())
}

/// 简单 token 校验（可选）。
fn authorized(state: &AdminState, token: Option<&str>) -> bool {
    match &state.admin_token {
        Some(t) => token == Some(t.as_str()),
        None => true,
    }
}

async fn status(State(s): State<AdminState>, headers: axum::http::HeaderMap) -> Json<Value> {
    if !authorized(&s, headers.get("x-admin-token").and_then(|v| v.to_str().ok())) {
        return Json(json!({"error": "unauthorized"}));
    }
    let usage = s.state.usage_bytes.read().await.clone();
    Json(json!({
        "status": "ok",
        "pid": std::process::id(),
        "uptime_s": s.started_at.elapsed().as_secs(),
        "tenants": s.tenants.count().await,
        "usage_bytes": usage,
        "quota_bytes": crate::state::quota_bytes(),
    }))
}

#[derive(Deserialize)]
struct AddTenant { domain: String, pubkey: String }

async fn add_tenant(
    State(s): State<AdminState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<AddTenant>,
) -> Json<Value> {
    if !authorized(&s, headers.get("x-admin-token").and_then(|v| v.to_str().ok())) {
        return Json(json!({"error": "unauthorized"}));
    }
    match s.tenants.upsert_tenant(&body.domain, &body.pubkey).await {
        Ok(()) => Json(json!({"ok": true, "tenant": body.domain})),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

async fn list_tenants(State(s): State<AdminState>, headers: axum::http::HeaderMap) -> Json<Value> {
    if !authorized(&s, headers.get("x-admin-token").and_then(|v| v.to_str().ok())) {
        return Json(json!({"error": "unauthorized"}));
    }
    let tenants = s.tenants.all().await;
    let usage = s.state.usage_bytes.read().await.clone();
    let list: Vec<Value> = tenants
        .iter()
        .map(|(domain, rec)| {
            json!({
                "domain": domain,
                "machines": rec.machines.len(),
                "usage_bytes": usage.get(domain).copied().unwrap_or(0),
            })
        })
        .collect();
    Json(json!({"tenants": list}))
}

async fn list_online(State(s): State<AdminState>, headers: axum::http::HeaderMap) -> Json<Value> {
    if !authorized(&s, headers.get("x-admin-token").and_then(|v| v.to_str().ok())) {
        return Json(json!({"error": "unauthorized"}));
    }
    Json(json!({"online": s.state.registry.all_online().await}))
}

async fn list_locks(State(s): State<AdminState>, headers: axum::http::HeaderMap) -> Json<Value> {
    if !authorized(&s, headers.get("x-admin-token").and_then(|v| v.to_str().ok())) {
        return Json(json!({"error": "unauthorized"}));
    }
    Json(json!({"locks": s.state.locks.snapshot().await}))
}

#[derive(Deserialize)]
struct LogQuery { lines: Option<usize> }

async fn logs(
    State(s): State<AdminState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<LogQuery>,
) -> Json<Value> {
    if !authorized(&s, headers.get("x-admin-token").and_then(|v| v.to_str().ok())) {
        return Json(json!({"error": "unauthorized"}));
    }
    let lines = q.lines.unwrap_or(200).min(5000);
    let content = match std::fs::read_to_string(&s.log_file) {
        Ok(c) => c.lines().rev().take(lines).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"),
        Err(e) => return Json(json!({"error": format!("log file {}: {e}", s.log_file.display())})),
    };
    Json(json!({"file": s.log_file.display().to_string(), "lines": lines, "content": content}))
}


// ── M6.2：用户系统（自助注册 / 登录）──

#[derive(Deserialize)]
struct RegisterReq { username: String, password: String }

async fn register_user(State(s): State<AdminState>, Json(b): Json<RegisterReq>) -> Json<Value> {
    let Some(db) = &s.state.db else {
        return Json(json!({"error": "user db not enabled (set NSMT_DB_URL)"}));
    };
    let username = b.username.trim().to_string();
    if username.is_empty() || username.len() > 64 || b.password.len() < 6 {
        return Json(json!({"error": "username 1-64 chars, password >= 6"}));
    }
    match db.register(&username, &b.password).await {
        Ok(token) => {
            // 自动创建租户（domain = username；公钥由客户端经 /api/tenants/key 登记）
            let _ = s.tenants.upsert_tenant(&username, "").await;
            Json(json!({"ok": true, "token": token, "domain": username, "quota_bytes": crate::db::UserDb::quota_for_plan("free")}))
        }
        Err(e) => Json(json!({"error": e})),
    }
}

#[derive(Deserialize)]
struct LoginReq { username: String, password: String }

async fn login_user(State(s): State<AdminState>, Json(b): Json<LoginReq>) -> Json<Value> {
    let Some(db) = &s.state.db else {
        return Json(json!({"error": "user db not enabled"}));
    };
    match db.login(&b.username, &b.password).await {
        Ok(token) => Json(json!({"ok": true, "token": token})),
        Err(e) => Json(json!({"error": e})),
    }
}

#[derive(Deserialize)]
struct SetKeyReq { domain: String, pubkey: String }

/// 自助注册用户登记其客户端域公钥（之后才能通过 AUTH）。
async fn set_tenant_key(State(s): State<AdminState>, Json(b): Json<SetKeyReq>) -> Json<Value> {
    if b.pubkey.len() != 64 {
        return Json(json!({"error": "pubkey must be 64 hex chars"}));
    }
    match s.tenants.upsert_tenant(&b.domain, &b.pubkey).await {
        Ok(()) => Json(json!({"ok": true, "domain": b.domain})),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}


// ── M8：会员 / 配额 UI ──

/// `GET /api/users`：用户列表 + plan + 用量 + 配额（配额 UI 数据源）。
async fn list_users(State(s): State<AdminState>, headers: axum::http::HeaderMap) -> Json<Value> {
    if !authorized(&s, headers.get("x-admin-token").and_then(|v| v.to_str().ok())) {
        return Json(json!({"error": "unauthorized"}));
    }
    let Some(db) = &s.state.db else {
        return Json(json!({"error": "user db not enabled"}));
    };
    let usage = s.state.usage_bytes.read().await.clone();
    match db.list_users().await {
        Ok(users) => {
            let list: Vec<Value> = users
                .iter()
                .map(|(u, plan, created)| {
                    let quota = crate::db::UserDb::quota_for_plan(plan);
                    json!({
                        "username": u,
                        "plan": plan,
                        "quota_bytes": quota,
                        "usage_bytes": usage.get(u).copied().unwrap_or(0),
                        "created_at": created,
                    })
                })
                .collect();
            Json(json!({"users": list}))
        }
        Err(e) => Json(json!({"error": e})),
    }
}

#[derive(Deserialize)]
struct UpgradeReq { plan: String }

/// `POST /api/users/{username}/upgrade`：会员升级（admin 操作，预留计费接口）。
async fn upgrade_user(
    State(s): State<AdminState>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(username): axum::extract::Path<String>,
    Json(b): Json<UpgradeReq>,
) -> Json<Value> {
    if !authorized(&s, headers.get("x-admin-token").and_then(|v| v.to_str().ok())) {
        return Json(json!({"error": "unauthorized"}));
    }
    let Some(db) = &s.state.db else {
        return Json(json!({"error": "user db not enabled"}));
    };
    match db.set_plan(&username, &b.plan).await {
        Ok(plan) => Json(json!({"ok": true, "username": username, "plan": plan, "quota_bytes": crate::db::UserDb::quota_for_plan(&plan)})),
        Err(e) => Json(json!({"error": e})),
    }
}


// ── M7：ygg-admin 需要的重启信号（进程重启由监督器执行）──

/// `POST /api/admin/restart`：请求优雅重启。返回后以退出码 3 退出，
/// 由 ygg-admin 监督器捕获并拉起新进程（M7.1）。
async fn admin_restart(State(s): State<AdminState>, headers: axum::http::HeaderMap) -> Json<Value> {
    if !authorized(&s, headers.get("x-admin-token").and_then(|v| v.to_str().ok())) {
        return Json(json!({"error": "unauthorized"}));
    }
    let pid = std::process::id();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        tracing::info!("admin restart requested; exiting with code 3");
        std::process::exit(3); // 监督器约定：3 = 请求重启
    });
    Json(json!({"ok": true, "restarting": true, "pid": pid}))
}


// ── 待办池：租户备份 / 恢复 ──

/// `GET /api/backup?domain=<d>`：把租户数据（trees/objects/tenants）打包到
/// `NSMT_HOME/backups/<domain>-<ts>.tar`，返回路径与条目数（仅本地后端可用）。
async fn backup_tenant(
    State(s): State<AdminState>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<Value> {
    if !authorized(&s, headers.get("x-admin-token").and_then(|v| v.to_str().ok())) {
        return Json(json!({"error": "unauthorized"}));
    }
    let Some(domain) = q.get("domain") else {
        return Json(json!({"error": "missing domain"}));
    };
    let nsmt_home = nsmt_home_path();
    let src = nsmt_home.join("server").join(sanitize(domain));
    if !src.exists() {
        return Json(json!({"error": format!("tenant dir not found: {}", src.display())}));
    }
    let backups = nsmt_home.join("backups");
    let _ = std::fs::create_dir_all(&backups);
    let ts = crate::registry::now_ms();
    let out = backups.join(format!("{domain}-{ts}.tar"));
    let count = tar_dir(&src, &out);
    Json(json!({
        "ok": count >= 0,
        "domain": domain,
        "archive": out.display().to_string(),
        "entries": count.max(0),
    }))
}

/// `POST /api/restore {archive, domain}`：从备份 tar 恢复租户数据。
#[derive(Deserialize)]
struct RestoreReq { archive: String, domain: String }

async fn restore_tenant(
    State(s): State<AdminState>,
    headers: axum::http::HeaderMap,
    Json(b): Json<RestoreReq>,
) -> Json<Value> {
    if !authorized(&s, headers.get("x-admin-token").and_then(|v| v.to_str().ok())) {
        return Json(json!({"error": "unauthorized"}));
    }
    let archive = std::path::PathBuf::from(&b.archive);
    if !archive.exists() {
        return Json(json!({"error": format!("archive not found: {}", archive.display())}));
    }
    let nsmt_home = nsmt_home_path();
    let dst = nsmt_home.join("server").join(sanitize(&b.domain));
    let count = untar_dir(&archive, &dst);
    Json(json!({"ok": count >= 0, "domain": b.domain, "restored_entries": count.max(0)}))
}

fn nsmt_home_path() -> std::path::PathBuf {
    std::env::var("NSMT_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(|h| std::path::PathBuf::from(h).join(".nsmt"))
                .unwrap_or_else(|_| std::path::PathBuf::from(".nsmt"))
        })
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

/// 简单 tar 打包（无外部依赖）：复制目录为扁平 tar（path + size + data）。
fn tar_dir(src: &std::path::Path, out: &std::path::Path) -> i64 {
    let mut count = 0i64;
    let mut buf = Vec::new();
    let mut files = Vec::new();
    collect_files(src, src, &mut files);
    for (rel, path) in &files {
        let Ok(data) = std::fs::read(path) else { continue };
        let rel_bytes = rel.as_bytes();
        buf.extend_from_slice(&(rel_bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
        buf.extend_from_slice(rel_bytes);
        buf.extend_from_slice(&data);
        count += 1;
    }
    match std::fs::write(out, &buf) {
        Ok(()) => count,
        Err(_) => -1,
    }
}

/// 解包 tar（tar_dir 的逆操作）。
fn untar_dir(archive: &std::path::Path, dst: &std::path::Path) -> i64 {
    let Ok(buf) = std::fs::read(archive) else { return -1 };
    let _ = std::fs::create_dir_all(dst);
    let mut i = 0usize;
    let mut count = 0i64;
    while i + 8 <= buf.len() {
        let rel_len = u32::from_le_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]) as usize;
        let data_len = u32::from_le_bytes([buf[i + 4], buf[i + 5], buf[i + 6], buf[i + 7]]) as usize;
        i += 8;
        if i + rel_len + data_len > buf.len() {
            break;
        }
        let rel = String::from_utf8_lossy(&buf[i..i + rel_len]).into_owned();
        let data = &buf[i + rel_len..i + rel_len + data_len];
        let target = dst.join(rel);
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::write(&target, data).is_ok() {
            count += 1;
        }
        i += rel_len + data_len;
    }
    count
}

fn collect_files(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(String, std::path::PathBuf)>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect_files(root, &p, out);
            } else if let Ok(rel) = p.strip_prefix(root) {
                out.push((rel.to_string_lossy().into_owned(), p));
            }
        }
    }
}
