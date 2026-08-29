//! 对象存储抽象（决策 #10：S3 兼容 MinIO/腾讯 COS 预留）。
//!
//! `ObjectStore` trait 定义 blob 异步读写；后端实现：
//! - `LocalObjectStore`：本地文件系统（默认）
//! - `S3ObjectStore`：S3 兼容（MinIO / 腾讯 COS），经 `object_store` crate
//! - `MemoryObjectStore`：内存（测试/开发）

use async_trait::async_trait;
use object_store::path::Path as ObjPath;
use object_store::{ObjectStore as OS, PutPayload};

/// 对象存储接口（异步）。
#[async_trait]
pub trait ObjectStore: Send + Sync {
    /// 写入对象（key 通常为 sha256 十六进制）。幂等：已存在则跳过。
    async fn put(&self, key: &str, bytes: &[u8]) -> std::io::Result<()>;
    /// 读取对象；不存在返回 `None`。
    async fn get(&self, key: &str) -> Option<Vec<u8>>;
    async fn exists(&self, key: &str) -> bool;
    async fn delete(&self, key: &str) -> std::io::Result<()>;
}

// ── 本地文件系统 ──

#[derive(Clone, Debug)]
pub struct LocalObjectStore {
    root: std::path::PathBuf,
}

impl LocalObjectStore {
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }
}

#[async_trait]
impl ObjectStore for LocalObjectStore {
    async fn put(&self, key: &str, bytes: &[u8]) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let p = self.root.join(key);
        if p.exists() {
            return Ok(());
        }
        std::fs::write(p, bytes)
    }
    async fn get(&self, key: &str) -> Option<Vec<u8>> {
        std::fs::read(self.root.join(key)).ok()
    }
    async fn exists(&self, key: &str) -> bool {
        self.root.join(key).exists()
    }
    async fn delete(&self, key: &str) -> std::io::Result<()> {
        std::fs::remove_file(self.root.join(key))
    }
}

// ── S3 兼容（MinIO / 腾讯 COS）──

/// S3 对象存储。配置来自环境变量：
///   NSMT_S3_ENDPOINT / NSMT_S3_REGION / NSMT_S3_BUCKET /
///   NSMT_S3_ACCESS_KEY / NSMT_S3_SECRET_KEY / NSMT_S3_HTTP（可选 true）
pub struct S3ObjectStore {
    inner: object_store::aws::AmazonS3,
}

impl S3ObjectStore {
    pub fn from_env() -> Result<Self, String> {
        let endpoint = std::env::var("NSMT_S3_ENDPOINT").map_err(|_| "NSMT_S3_ENDPOINT 未设置".to_string())?;
        let bucket = std::env::var("NSMT_S3_BUCKET").map_err(|_| "NSMT_S3_BUCKET 未设置".to_string())?;
        let region = std::env::var("NSMT_S3_REGION").unwrap_or_else(|_| "us-east-1".into());
        let access = std::env::var("NSMT_S3_ACCESS_KEY").map_err(|_| "NSMT_S3_ACCESS_KEY 未设置".to_string())?;
        let secret = std::env::var("NSMT_S3_SECRET_KEY").map_err(|_| "NSMT_S3_SECRET_KEY 未设置".to_string())?;
        let allow_http = std::env::var("NSMT_S3_HTTP").is_ok();

        let mut b = object_store::aws::AmazonS3Builder::new()
            .with_endpoint(endpoint)
            .with_region(region)
            .with_bucket_name(bucket)
            .with_access_key_id(access)
            .with_secret_access_key(secret);
        if allow_http {
            b = b.with_allow_http(true);
        }
        let inner = b.build().map_err(|e| e.to_string())?;
        Ok(Self { inner })
    }
}

#[async_trait]
impl ObjectStore for S3ObjectStore {
    async fn put(&self, key: &str, bytes: &[u8]) -> std::io::Result<()> {
        self.inner
            .put(&ObjPath::from(key), PutPayload::from(bytes.to_vec()))
            .await
            .map(|_| ())
            .map_err(io_other)
    }
    async fn get(&self, key: &str) -> Option<Vec<u8>> {
        match self.inner.get(&ObjPath::from(key)).await {
            Ok(r) => r.bytes().await.ok().map(|b| b.to_vec()),
            Err(_) => None,
        }
    }
    async fn exists(&self, key: &str) -> bool {
        self.inner.head(&ObjPath::from(key)).await.is_ok()
    }
    async fn delete(&self, key: &str) -> std::io::Result<()> {
        self.inner.delete(&ObjPath::from(key)).await.map(|_| ()).map_err(io_other)
    }
}

// ── 内存（测试/开发）──

#[derive(Clone)]
pub struct MemoryObjectStore {
    inner: std::sync::Arc<object_store::memory::InMemory>,
}

impl Default for MemoryObjectStore {
    fn default() -> Self {
        Self { inner: std::sync::Arc::new(object_store::memory::InMemory::new()) }
    }
}

#[async_trait]
impl ObjectStore for MemoryObjectStore {
    async fn put(&self, key: &str, bytes: &[u8]) -> std::io::Result<()> {
        self.inner
            .put(&ObjPath::from(key), PutPayload::from(bytes.to_vec()))
            .await
            .map(|_| ())
            .map_err(io_other)
    }
    async fn get(&self, key: &str) -> Option<Vec<u8>> {
        match self.inner.get(&ObjPath::from(key)).await {
            Ok(r) => r.bytes().await.ok().map(|b| b.to_vec()),
            Err(_) => None,
        }
    }
    async fn exists(&self, key: &str) -> bool {
        self.inner.head(&ObjPath::from(key)).await.is_ok()
    }
    async fn delete(&self, key: &str) -> std::io::Result<()> {
        self.inner.delete(&ObjPath::from(key)).await.map(|_| ()).map_err(io_other)
    }
}

fn io_other(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
}

/// 给任意后端加 key 前缀（M9.3：S3 多租户，每租户 `t/<domain>/objects/`）。
#[derive(Clone)]
pub struct PrefixedObjectStore {
    inner: std::sync::Arc<dyn ObjectStore>,
    prefix: String,
}

impl PrefixedObjectStore {
    pub fn new(inner: std::sync::Arc<dyn ObjectStore>, prefix: impl Into<String>) -> Self {
        let p = prefix.into();
        let p = if p.is_empty() || p.ends_with('/') { p } else { format!("{p}/") };
        Self { inner, prefix: p }
    }
    pub fn key(&self, k: &str) -> String {
        format!("{}{}", self.prefix, k.trim_start_matches('/'))
    }
}

#[async_trait]
impl ObjectStore for PrefixedObjectStore {
    async fn put(&self, key: &str, bytes: &[u8]) -> std::io::Result<()> {
        self.inner.put(&self.key(key), bytes).await
    }
    async fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.inner.get(&self.key(key)).await
    }
    async fn exists(&self, key: &str) -> bool {
        self.inner.exists(&self.key(key)).await
    }
    async fn delete(&self, key: &str) -> std::io::Result<()> {
        self.inner.delete(&self.key(key)).await
    }
}

/// 从环境选择后端：`NSMT_OBJECT_STORE=s3` → S3；`memory` → 内存；否则本地。
pub fn from_env_or(root: std::path::PathBuf) -> Box<dyn ObjectStore> {
    match std::env::var("NSMT_OBJECT_STORE").as_deref() {
        Ok("s3") => match S3ObjectStore::from_env() {
            Ok(s) => Box::new(s),
            Err(e) => {
                tracing::warn!("S3 backend init failed ({e}); fallback to local");
                Box::new(LocalObjectStore::new(root))
            }
        },
        Ok("memory") => Box::new(MemoryObjectStore::default()),
        _ => Box::new(LocalObjectStore::new(root)),
    }
}

/// 按租户取对象存储（M9.3）：本地后端按根目录隔离；S3/内存后端共享底层，
/// 用 `t/<domain>/objects/` 前缀做租户隔离。
pub fn from_env_tenant(
    root: std::path::PathBuf,
    user_domain: &str,
) -> Box<dyn ObjectStore> {
    let base: Box<dyn ObjectStore> = match std::env::var("NSMT_OBJECT_STORE").as_deref() {
        Ok("s3") => match S3ObjectStore::from_env() {
            Ok(s) => Box::new(s),
            Err(e) => {
                tracing::warn!("S3 backend init failed ({e}); fallback to local");
                Box::new(LocalObjectStore::new(root))
            }
        },
        Ok("memory") => Box::new(MemoryObjectStore::default()),
        _ => Box::new(LocalObjectStore::new(root)),
    };
    match std::env::var("NSMT_OBJECT_STORE").as_deref() {
        // 本地后端已按 root（server/<domain>/objects）隔离；共享后端需前缀
        Ok("s3") | Ok("memory") => Box::new(PrefixedObjectStore::new(
            std::sync::Arc::from(base),
            format!("t/{user_domain}/objects"),
        )),
        _ => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_store_roundtrip() {
        let dir = std::env::temp_dir().join(format!("nsmt-obj-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = LocalObjectStore::new(&dir);
        store.put("abc", b"hello").await.unwrap();
        assert!(store.exists("abc").await);
        assert_eq!(store.get("abc").await, Some(b"hello".to_vec()));
        assert!(!store.exists("nope").await);
        store.delete("abc").await.unwrap();
        assert!(!store.exists("abc").await);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn memory_store_roundtrip() {
        let store = MemoryObjectStore::default();
        store.put("k", b"v").await.unwrap();
        assert_eq!(store.get("k").await, Some(b"v".to_vec()));
        assert!(store.exists("k").await);
    }

    #[tokio::test]
    async fn prefixed_store_isolates_tenants() {
        let base = MemoryObjectStore::default();
        let a = PrefixedObjectStore::new(std::sync::Arc::new(base.clone()), "t/domain-a/objects");
        let b = PrefixedObjectStore::new(std::sync::Arc::new(base.clone()), "t/domain-b/objects");
        a.put("obj1", b"aaa").await.unwrap();
        b.put("obj2", b"bbb").await.unwrap();
        assert_eq!(a.get("obj1").await, Some(b"aaa".to_vec()));
        assert_eq!(a.get("obj2").await, None);
        assert_eq!(b.get("obj2").await, Some(b"bbb".to_vec()));
        assert_eq!(b.get("obj1").await, None);
        assert!(a.exists("obj1").await);
        assert!(!a.exists("obj2").await);
    }
}
