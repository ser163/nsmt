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
}

impl ServerState {
    pub fn new() -> Self {
        Self {
            registry: Arc::new(Registry::default()),
            locks: Arc::new(LockRegistry::default()),
            pool: crate::memory::pool_from_env(),
        }
    }
}
