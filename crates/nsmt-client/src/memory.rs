//! 客户端记忆（protocol.md §6）：网络优先读 + 本地托底回退；写双写。
//!
//! - 主：域池（经服务器 MEMORY_RECALL/CAPTURE）
//! - 托底：本地腾讯 Gateway（NSMT_LOCAL_GATEWAY，默认 127.0.0.1:8420）

use nsmt_core::messages::{MemoryHit, MemoryRecallResult};
use nsmt_memory::{MemoryError, TencentClient};
use std::time::Duration;

/// 本地托底记忆库。
#[derive(Clone)]
pub struct LocalFallback {
    local: TencentClient,
}

impl LocalFallback {
    pub fn from_env() -> Self {
        let base = std::env::var("NSMT_LOCAL_GATEWAY")
            .unwrap_or_else(|_| "http://127.0.0.1:8420".into());
        Self {
            local: TencentClient::new(base).with_timeout(Duration::from_millis(1500)),
        }
    }

    pub fn base_url(&self) -> String {
        self.local.base_url().to_string()
    }

    /// 本地召回（托底）。
    pub async fn recall_local(&self, query: &str, request_id: &str) -> Result<MemoryRecallResult, MemoryError> {
        let resp = self.local.recall(query, &format!("local:{request_id}")).await?;
        let mut memories = Vec::new();
        if !resp.context.trim().is_empty() {
            memories.push(MemoryHit {
                content: resp.context,
                fqn: "local".into(),
                score: 0.0,
                scope: "machine".into(),
            });
        }
        Ok(MemoryRecallResult {
            request_id: request_id.to_string(),
            source: "local".into(),
            memories,
            latency_ms: 0,
        })
    }

    /// 本地写入（托底，双写之备）。
    pub async fn capture_local(
        &self,
        user: &str,
        assistant: &str,
        fqn: &str,
    ) -> Result<(), MemoryError> {
        self.local.capture(user, assistant, fqn, None).await?;
        Ok(())
    }
}
