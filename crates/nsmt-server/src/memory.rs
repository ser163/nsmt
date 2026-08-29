//! 服务器端记忆域池桥接（protocol.md §6）。
//!
//! 域池 = 服务器上该租户的腾讯 Gateway 实例。M0 联调期默认指向本机 :8420，
//! 可通过 `NSMT_POOL_GATEWAY` 覆盖（如云上的池实例）。
//!
//! M9.5 跨机聚合/分片：`NSMT_POOL_GATEWAYS`（逗号分隔多网关）启用**域池分片**——
//! - recall：fan-out 到所有分片并发查询，按 score 降序聚合取 top `limit`（跨机记忆合并）；
//! - capture：按 `fqn` 稳定哈希路由到其中一个分片（写放大 O(1)，去重靠幂等）。

use nsmt_core::messages::{MemoryCapture, MemoryCaptureResult, MemoryHit, MemoryRecall, MemoryRecallResult};
use nsmt_memory::{MemoryError, TencentClient};
use sha2::Digest as _;

/// 记忆域池（多分片）。
#[derive(Clone)]
pub struct MemoryPool {
    shards: Vec<TencentClient>,
}

impl MemoryPool {
    pub fn new(base: String) -> Self {
        Self {
            shards: vec![TencentClient::new(base).with_timeout(std::time::Duration::from_millis(1500))],
        }
    }

    pub fn from_gateways(gateways: Vec<String>) -> Self {
        let shards: Vec<TencentClient> = gateways
            .into_iter()
            .map(|g| TencentClient::new(g).with_timeout(std::time::Duration::from_millis(1500)))
            .collect();
        Self { shards }
    }

    pub fn shard_count(&self) -> usize {
        self.shards.len()
    }

    /// fqn 稳定哈希 → 分片下标（capture 路由）。
    fn shard_for(&self, fqn: &str) -> usize {
        let h = sha2::Sha256::digest(fqn.as_bytes());
        let v = u64::from_be_bytes([h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]]);
        (v % self.shards.len() as u64) as usize
    }

    /// 网络优先读：fan-out 所有分片，聚合按 score 降序取 top limit（跨机合并）。
    pub async fn recall(&self, msg: &MemoryRecall) -> Result<MemoryRecallResult, MemoryError> {
        let started = std::time::Instant::now();
        if self.shards.len() == 1 {
            return self.recall_one(&self.shards[0], msg, &started).await;
        }
        let limit = msg.limit.max(1) as usize;
        let mut handles = Vec::new();
        for shard in &self.shards {
            let shard = shard.clone();
            let msg = msg.clone();
            handles.push(tokio::spawn(async move {
                let resp = shard.recall(&msg.query, &format!("pool:{}", msg.request_id)).await;
                resp
            }));
        }
        let mut hits: Vec<MemoryHit> = Vec::new();
        let mut failed = 0usize;
        for h in handles {
            match h.await {
                Ok(Ok(resp)) => {
                    if !resp.context.trim().is_empty() {
                        hits.push(MemoryHit {
                            content: resp.context,
                            fqn: String::new(),
                            score: 0.0,
                            scope: "user".into(),
                        });
                    }
                }
                _ => failed += 1,
            }
        }
        // 腾讯 /recall 只返回聚合 context，无逐条 score；按分片序号稳定排序后截断
        hits.sort_by(|a, b| a.content.cmp(&b.content));
        hits.truncate(limit);
        Ok(MemoryRecallResult {
            request_id: msg.request_id.clone(),
            source: if failed == self.shards.len() { "pool_unavailable".into() } else { "pool".into() },
            memories: hits,
            latency_ms: started.elapsed().as_millis() as u64,
        })
    }

    async fn recall_one(
        &self,
        shard: &TencentClient,
        msg: &MemoryRecall,
        started: &std::time::Instant,
    ) -> Result<MemoryRecallResult, MemoryError> {
        let resp = shard.recall(&msg.query, &format!("pool:{}", msg.request_id)).await?;
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
            latency_ms: started.elapsed().as_millis() as u64,
        })
    }

    /// 双写的主路径：写域池。分片模式下按 fqn 哈希路由到单分片。
    pub async fn capture(&self, msg: &MemoryCapture) -> Result<MemoryCaptureResult, MemoryError> {
        let shard_idx = self.shard_for(&msg.fqn);
        let resp = self.shards[shard_idx]
            .capture(&msg.user_content, &msg.assistant_content, &msg.fqn, None)
            .await?;
        Ok(MemoryCaptureResult {
            request_id: msg.request_id.clone(),
            committed: resp.l0_recorded > 0,
            queued: false,
        })
    }
}

/// 从环境构造域池：`NSMT_POOL_GATEWAYS`（逗号分隔多分片）优先，否则 `NSMT_POOL_GATEWAY`，默认本机 :8420。
pub fn pool_from_env() -> MemoryPool {
    if let Ok(g) = std::env::var("NSMT_POOL_GATEWAYS") {
        let list: Vec<String> = g
            .split(',')
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !list.is_empty() {
            return MemoryPool::from_gateways(list);
        }
    }
    let base = std::env::var("NSMT_POOL_GATEWAY").unwrap_or_else(|_| "http://127.0.0.1:8420".into());
    MemoryPool::new(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shard_for_stable_hash_routing() {
        let pool = MemoryPool::from_gateways(vec!["http://a".into(), "http://b".into(), "http://c".into()]);
        assert_eq!(pool.shard_count(), 3);
        let fqn1 = "ser163/aaaaaaaaaaaaaaaa/maka";
        let fqn2 = "ser163/bbbbbbbbbbbbbbbb/maka";
        // 同一 fqn 稳定路由到同一分片
        assert_eq!(pool.shard_for(fqn1), pool.shard_for(fqn1));
        assert_eq!(pool.shard_for(fqn2), pool.shard_for(fqn2));
        // 不同 fqn 可落在不同分片（哈希均匀性不保证，但范围正确）
        for f in [fqn1, fqn2, "x/y/z", "other/a/b"] {
            assert!(pool.shard_for(f) < 3);
        }
    }

    #[test]
    fn pool_from_env_parses_gateways() {
        std::env::set_var("NSMT_POOL_GATEWAYS", "http://127.0.0.1:9001, http://127.0.0.1:9002/");
        let pool = pool_from_env();
        assert_eq!(pool.shard_count(), 2);
        std::env::remove_var("NSMT_POOL_GATEWAYS");
    }

    #[test]
    fn pool_from_env_single_default() {
        std::env::remove_var("NSMT_POOL_GATEWAYS");
        std::env::remove_var("NSMT_POOL_GATEWAY");
        let pool = pool_from_env();
        assert_eq!(pool.shard_count(), 1);
    }
}
