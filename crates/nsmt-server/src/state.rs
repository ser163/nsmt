//! 服务器共享状态。

pub use crate::fs::LockRegistry;
pub use crate::db::UserDb;
pub use crate::memory::MemoryPool;
pub use crate::registry::Registry;
use std::sync::Arc;

/// 全局服务器状态（跨连接共享）。
#[derive(Clone)]
pub struct ServerState {
    pub registry: Arc<Registry>,
    pub locks: Arc<LockRegistry>,
    pub pool: MemoryPool,
    /// 用户数据库（SQLite/MySQL/PG，经 NSMT_DB_URL；可选）。
    pub db: Option<Arc<UserDb>>,
    /// 按租户缓存的对象存储（内存后端必须跨连接共享）。
    pub object_stores: Arc<tokio::sync::RwLock<std::collections::HashMap<String, Arc<dyn nsmt_fs::ObjectStore>>>>,
    /// 对象 → 上传机器（P2P 路由用）。
    pub object_owners: Arc<tokio::sync::RwLock<std::collections::HashMap<String, std::collections::HashMap<String, String>>>>,
    /// 每租户已用字节（配额）。
    pub usage_bytes: Arc<tokio::sync::RwLock<std::collections::HashMap<String, u64>>>,
}

/// 租户配额（默认 1 GiB）。
pub fn quota_bytes() -> u64 {
    std::env::var("NSMT_QUOTA_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1024 * 1024 * 1024)
}

impl ServerState {
    pub async fn new() -> Self {
        let db = UserDb::connect(std::env::var("NSMT_DB_URL").ok().as_deref())
            .await
            .map(Arc::new)
            .map_err(|e| tracing::warn!("user DB unavailable (NSMT_DB_URL): {e}"))
            .ok();
        Self {
            registry: Arc::new(Registry::default()),
            locks: Arc::new(LockRegistry::default()),
            pool: crate::memory::pool_from_env(),
            db,
            object_stores: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
            object_owners: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
            usage_bytes: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        }
    }

    /// 某租户配额：优先按用户 plan（M6.3），否则全局 env。
    pub async fn quota_for(&self, user_domain: &str) -> u64 {
        if let Some(db) = &self.db {
            match db.plan(user_domain).await {
                Ok(p) => UserDb::quota_for_plan(&p),
                Err(_) => UserDb::quota_for_plan("free"),
            }
        } else {
            quota_bytes()
        }
    }

    /// 检查并预占配额（对象尚未存储时）。超限返回 false。
    pub async fn try_reserve_quota(&self, user_domain: &str, size: u64) -> bool {
        let limit = self.quota_for(user_domain).await;
        let mut g = self.usage_bytes.write().await;
        let used = g.get(user_domain).copied().unwrap_or(0);
        if used + size > limit {
            return false;
        }
        *g.entry(user_domain.to_string()).or_default() += size;
        true
    }

    /// 记录某对象归属的机器（P2P 用）。
    pub async fn record_object_owner(&self, user_domain: &str, blob_id: &str, machine: &str) {
        let mut g = self.object_owners.write().await;
        g.entry(user_domain.to_string()).or_default().insert(blob_id.to_string(), machine.to_string());
    }

    /// 查对象归属机器。
    pub async fn object_owner(&self, user_domain: &str, blob_id: &str) -> Option<String> {
        let g = self.object_owners.read().await;
        g.get(user_domain).and_then(|m| m.get(blob_id)).cloned()
    }

    /// 取（或创建）某租户的对象存储。
    pub async fn object_store_for(&self, user_domain: &str, base: &std::path::Path) -> Arc<dyn nsmt_fs::ObjectStore> {
        let mut g = self.object_stores.write().await;
        if let Some(s) = g.get(user_domain) {
            return s.clone();
        }
        let root = base.join("server").join(sanitize_domain(user_domain)).join("objects");
        let store: Arc<dyn nsmt_fs::ObjectStore> = Arc::from(nsmt_fs::from_env_or(root));
        g.insert(user_domain.to_string(), store.clone());
        store
    }
}

fn sanitize_domain(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}
