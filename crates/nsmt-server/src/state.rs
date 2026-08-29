//! 服务器共享状态。

pub use crate::fs::LockRegistry;
pub use crate::memory::MemoryPool;
pub use crate::registry::Registry;
use std::sync::Arc;

/// 全局服务器状态（跨连接共享）。
#[derive(Clone)]
pub struct ServerState {
    pub registry: Arc<Registry>,
    pub locks: Arc<LockRegistry>,
    pub pool: MemoryPool,
    /// 按租户缓存的对象存储（内存后端必须跨连接共享）。
    pub object_stores: Arc<tokio::sync::RwLock<std::collections::HashMap<String, Arc<dyn nsmt_fs::ObjectStore>>>>,
    /// 对象 → 上传机器（P2P 路由用）。
    pub object_owners: Arc<tokio::sync::RwLock<std::collections::HashMap<String, std::collections::HashMap<String, String>>>>,
}

impl ServerState {
    pub fn new() -> Self {
        Self {
            registry: Arc::new(Registry::default()),
            locks: Arc::new(LockRegistry::default()),
            pool: crate::memory::pool_from_env(),
            object_stores: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
            object_owners: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        }
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
