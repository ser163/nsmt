//! 用户数据库（M6.2）—— 通用 SQL 驱动（sqlx Any）：SQLite / MySQL / PostgreSQL。
//!
//! 通过 `NSMT_DB_URL` 选择驱动：
//!   sqlite://~/.nsmt/users.db?mode=rwc   （默认，自动建库）
//!   postgres://user:pass@host/db
//!   mysql://user:pass@host/db
//!
//! 表：users / sessions / invites / usage。密码用 argon2。

use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use sqlx::any::{Any, AnyPoolOptions};
use sqlx::{AnyPool, Row};
use std::str::FromStr;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbKind {
    Sqlite,
    MySql,
    Postgres,
}

/// 默认免费用户配额（50 MB）。
pub const DEFAULT_FREE_QUOTA_BYTES: u64 = 50 * 1024 * 1024;
/// 会员配额（预留，后续 plan 扩展）。
pub const PRO_QUOTA_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone)]
pub struct UserDb {
    pool: AnyPool,
    kind: DbKind,
}

/// 默认 SQLite 数据库路径。
pub fn default_sqlite_url() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let dir = std::env::var("NSMT_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(&home).join(".nsmt"));
    let _ = std::fs::create_dir_all(&dir);
    format!(
        "sqlite://{}?mode=rwc",
        dir.join("users.db").display()
    )
}

impl UserDb {
    /// 从连接串连接并初始化。`url` 为空则用默认 SQLite。
    pub async fn connect(url: Option<&str>) -> Result<Self, sqlx::Error> {
        // 注册 Any 驱动（sqlite/mysql/postgres，取决于编译特性）
        let _ = sqlx::any::install_default_drivers();
        let owned_default;
        let url = match url {
            Some(u) => u,
            None => { owned_default = default_sqlite_url(); &owned_default }
        };
        let kind = if url.starts_with("postgres") {
            DbKind::Postgres
        } else if url.starts_with("mysql") {
            DbKind::MySql
        } else {
            DbKind::Sqlite
        };
        let pool = AnyPoolOptions::new()
            .max_connections(8)
            .connect(url)
            .await?;
        let db = Self { pool, kind };
        db.init().await?;
        Ok(db)
    }

    pub fn kind(&self) -> DbKind {
        self.kind
    }

    async fn init(&self) -> Result<(), sqlx::Error> {
        let users = match self.kind {
            DbKind::Sqlite => "CREATE TABLE IF NOT EXISTS users (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                username TEXT NOT NULL UNIQUE,
                password_hash TEXT NOT NULL,
                domain TEXT NOT NULL UNIQUE,
                plan TEXT NOT NULL DEFAULT 'free',
                created_at INTEGER NOT NULL DEFAULT 0)",
            DbKind::MySql => "CREATE TABLE IF NOT EXISTS users (
                id INT AUTO_INCREMENT PRIMARY KEY,
                username VARCHAR(128) NOT NULL UNIQUE,
                password_hash TEXT NOT NULL,
                domain VARCHAR(128) NOT NULL UNIQUE,
                plan VARCHAR(32) NOT NULL DEFAULT 'free',
                created_at BIGINT NOT NULL DEFAULT 0)",
            DbKind::Postgres => "CREATE TABLE IF NOT EXISTS users (
                id BIGSERIAL PRIMARY KEY,
                username TEXT NOT NULL UNIQUE,
                password_hash TEXT NOT NULL,
                domain TEXT NOT NULL UNIQUE,
                plan TEXT NOT NULL DEFAULT 'free',
                created_at BIGINT NOT NULL DEFAULT 0)",
        };
        let sessions = match self.kind {
            DbKind::Sqlite => "CREATE TABLE IF NOT EXISTS sessions (
                token TEXT PRIMARY KEY,
                user_id INTEGER NOT NULL,
                expires_at INTEGER NOT NULL)",
            DbKind::MySql => "CREATE TABLE IF NOT EXISTS sessions (
                token VARCHAR(128) PRIMARY KEY,
                user_id INT NOT NULL,
                expires_at BIGINT NOT NULL)",
            DbKind::Postgres => "CREATE TABLE IF NOT EXISTS sessions (
                token TEXT PRIMARY KEY,
                user_id BIGINT NOT NULL,
                expires_at BIGINT NOT NULL)",
        };
        sqlx::query(users).execute(&self.pool).await?;
        sqlx::query(sessions).execute(&self.pool).await?;
        Ok(())
    }

    /// 注册用户：哈希密码、建租户（domain = username）、发会话 token。
    pub async fn register(&self, username: &str, password: &str) -> Result<String, String> {
        let salt = SaltString::generate(&mut OsRng);
        let hash = Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map_err(|e| format!("hash: {e}"))?
            .to_string();
        let now = crate::registry::now_ms();
        let inserted = sqlx::query("INSERT INTO users (username, password_hash, domain, plan, created_at) VALUES (?, ?, ?, 'free', ?)")
            .bind(username)
            .bind(&hash)
            .bind(username)
            .bind(now as i64)
            .execute(&self.pool)
            .await;
        if let Err(e) = inserted {
            return Err(format!("注册失败（可能用户名已存在）: {e}"));
        }
        Ok(self.issue_token(username).await)
    }

    /// 登录：校验密码，成功发 token。
    pub async fn login(&self, username: &str, password: &str) -> Result<String, String> {
        let row = sqlx::query("SELECT password_hash FROM users WHERE username = ?")
            .bind(username)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| format!("db: {e}"))?;
        let Some(row) = row else { return Err("用户不存在".into()) };
        let hash: String = row.try_get("password_hash").map_err(|e| format!("{e}"))?;
        let parsed = PasswordHash::new(&hash).map_err(|e| format!("hash parse: {e}"))?;
        if Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
        {
            Ok(self.issue_token(username).await)
        } else {
            Err("密码错误".into())
        }
    }

    /// 查询用户 plan（决定配额）。
    pub async fn plan(&self, domain: &str) -> Result<String, String> {
        let row = sqlx::query("SELECT plan FROM users WHERE domain = ?")
            .bind(domain)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| format!("db: {e}"))?;
        match row {
            Some(r) => r.try_get("plan").map_err(|e| format!("{e}")),
            None => Ok("free".into()),
        }
    }

    /// plan → 配额字节。
    pub fn quota_for_plan(plan: &str) -> u64 {
        match plan {
            "pro" => PRO_QUOTA_BYTES,
            _ => DEFAULT_FREE_QUOTA_BYTES,
        }
    }

    async fn issue_token(&self, username: &str) -> String {
        use sha2::{Digest, Sha256};
        let token = format!(
            "t-{}-{}",
            username,
            hex::encode(Sha256::digest(format!("{}:{}", username, crate::registry::now_ms()).as_bytes()))
        );
        let now = crate::registry::now_ms();
        let _ = sqlx::query("INSERT INTO sessions (token, user_id, expires_at) SELECT ?, id, ? FROM users WHERE username = ?")
            .bind(&token)
            .bind((now + 7 * 24 * 3600 * 1000) as i64)
            .bind(username)
            .execute(&self.pool)
            .await;
        token
    }
}
