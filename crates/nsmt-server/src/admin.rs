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
