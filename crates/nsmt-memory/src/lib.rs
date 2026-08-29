//! 腾讯记忆 Gateway HTTP 桥接（protocol.md §6）。
//!
//! 封装腾讯 Gateway 的 v1 HTTP API：
//! - `POST /recall`            记忆召回
//! - `POST /capture`           对话捕获（双写之一）
//! - `POST /search/memories`   L1 记忆检索
//! - `GET  /health`            健康检查
//!
//! 一个 `TencentClient` 指向一个 Gateway 实例：域池（主）或本地托底（备）。

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// 腾讯 Gateway v1 召回响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallResp {
    pub context: String,
    #[serde(default)]
    pub strategy: String,
    #[serde(default)]
    pub memory_count: u32,
    #[serde(default)]
    pub code: i64,
    #[serde(default)]
    pub message: String,
}

/// 腾讯 Gateway v1 捕获响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureResp {
    #[serde(default)]
    pub l0_recorded: u32,
    #[serde(default)]
    pub scheduler_notified: bool,
}

/// 腾讯 Gateway v1 记忆检索响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResp {
    #[serde(default)]
    pub results: String,
    #[serde(default)]
    pub total: u32,
    #[serde(default)]
    pub strategy: String,
}

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("gateway error code {code}: {msg}")]
    Gateway { code: i64, msg: String },
    #[error("timeout")]
    Timeout,
}

/// 腾讯 Gateway HTTP 客户端。
#[derive(Debug, Clone)]
pub struct TencentClient {
    base: String,
    http: reqwest::Client,
    timeout: Duration,
}

impl TencentClient {
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
            timeout: Duration::from_millis(1500),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// 健康检查。
    pub async fn health(&self) -> Result<serde_json::Value, MemoryError> {
        self.http
            .get(format!("{}/health", self.base))
            .timeout(self.timeout)
            .send()
            .await?
            .json()
            .await
            .map_err(Into::into)
    }

    /// 记忆召回（网络优先读的主路径）。
    pub async fn recall(
        &self,
        query: &str,
        session_key: &str,
    ) -> Result<RecallResp, MemoryError> {
        let resp = self
            .http
            .post(format!("{}/recall", self.base))
            .json(&serde_json::json!({ "query": query, "session_key": session_key }))
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    MemoryError::Timeout
                } else {
                    MemoryError::Http(e)
                }
            })?
            .json::<RecallResp>()
            .await?;
        if resp.code != 0 {
            return Err(MemoryError::Gateway {
                code: resp.code,
                msg: resp.message.clone(),
            });
        }
        Ok(resp)
    }

    /// 对话捕获（双写之一）。
    pub async fn capture(
        &self,
        user_content: &str,
        assistant_content: &str,
        session_key: &str,
        session_id: Option<&str>,
    ) -> Result<CaptureResp, MemoryError> {
        let mut body = serde_json::json!({
            "user_content": user_content,
            "assistant_content": assistant_content,
            "session_key": session_key,
        });
        if let Some(sid) = session_id {
            body["session_id"] = serde_json::json!(sid);
        }
        let resp = self
            .http
            .post(format!("{}/capture", self.base))
            .json(&body)
            .timeout(self.timeout)
            .send()
            .await?
            .json::<CaptureResp>()
            .await?;
        Ok(resp)
    }

    /// L1 记忆检索。
    pub async fn search_memories(
        &self,
        query: &str,
        limit: u32,
    ) -> Result<SearchResp, MemoryError> {
        let resp = self
            .http
            .post(format!("{}/search/memories", self.base))
            .json(&serde_json::json!({ "query": query, "limit": limit }))
            .timeout(self.timeout)
            .send()
            .await?
            .json::<SearchResp>()
            .await?;
        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_trims_slash() {
        let c = TencentClient::new("http://127.0.0.1:8420/");
        assert_eq!(c.base_url(), "http://127.0.0.1:8420");
    }
}
