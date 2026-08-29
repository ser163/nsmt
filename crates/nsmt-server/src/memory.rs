//! 服务器端记忆域池桥接（protocol.md §6）。
//!
//! 域池 = 服务器上该租户的腾讯 Gateway 实例。M0 联调期默认指向本机 :8420，
//! 可通过 `NSMT_POOL_GATEWAY` 覆盖（如云上的池实例）。

use nsmt_core::messages::{MemoryCapture, MemoryCaptureResult, MemoryHit, MemoryRecall, MemoryRecallResult};
use nsmt_memory::{MemoryError, TencentClient};

/// 记忆域池。
#[derive(Clone)]
pub struct MemoryPool {
    pool: TencentClient,
}

impl MemoryPool {
    pub fn new(base: String) -> Self {
        Self {
            pool: TencentClient::new(base).with_timeout(std::time::Duration::from_millis(1500)),
        }
    }

    /// 网络优先读：查域池。
    pub async fn recall(&self, msg: &MemoryRecall) -> Result<MemoryRecallResult, MemoryError> {
        let resp = self.pool.recall(&msg.query, &format!("pool:{}", msg.request_id)).await?;
        // 腾讯 /recall 返回 context 文本；这里直接包装为一条记忆上下文
        let mut memories = Vec::new();
        if !resp.context.trim().is_empty() {
            memories.push(MemoryHit {
                content: resp.context,
                fqn: String::new(),
                score: 0.0,
                scope: "user".into(),
            });
        }
        Ok(MemoryRecallResult {
            request_id: msg.request_id.clone(),
            source: "pool".into(),
            memories,
            latency_ms: 0,
        })
    }

    /// 双写的主路径：写域池。
    pub async fn capture(&self, msg: &MemoryCapture) -> Result<MemoryCaptureResult, MemoryError> {
        let resp = self
            .pool
            .capture(&msg.user_content, &msg.assistant_content, &msg.fqn, None)
            .await?;
        Ok(MemoryCaptureResult {
            request_id: msg.request_id.clone(),
            committed: resp.l0_recorded > 0,
            queued: false,
        })
    }
}

/// 从环境构造域池（默认本机 :8420）。
pub fn pool_from_env() -> MemoryPool {
    let base = std::env::var("NSMT_POOL_GATEWAY").unwrap_or_else(|_| "http://127.0.0.1:8420".into());
    MemoryPool::new(base)
}
