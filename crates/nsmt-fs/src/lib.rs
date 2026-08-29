//! 对象存储抽象（决策 #10：S3 兼容 MinIO/COS 预留）。
//!
//! `ObjectStore` trait 定义 blob 读写；`LocalObjectStore` 为文件系统实现。
//! 后续按决策 #10 用 `object_store` crate 接入 S3（MinIO/腾讯 COS）时，只需新增实现。

use std::path::{Path, PathBuf};

/// 对象存储接口。
pub trait ObjectStore: Send + Sync {
    /// 写入对象（key 通常为 sha256 十六进制）。幂等：已存在则跳过。
    fn put(&self, key: &str, bytes: &[u8]) -> std::io::Result<()>;
    /// 读取对象；不存在返回 `None`。
    fn get(&self, key: &str) -> Option<Vec<u8>>;
    fn exists(&self, key: &str) -> bool;
    fn delete(&self, key: &str) -> std::io::Result<()>;
}

/// 本地文件系统对象存储：`<root>/<key>`。
#[derive(Clone, Debug)]
pub struct LocalObjectStore {
    root: PathBuf,
}

impl LocalObjectStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl ObjectStore for LocalObjectStore {
    fn put(&self, key: &str, bytes: &[u8]) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let p = self.root.join(key);
        if p.exists() {
            return Ok(());
        }
        std::fs::write(p, bytes)
    }

    fn get(&self, key: &str) -> Option<Vec<u8>> {
        std::fs::read(self.root.join(key)).ok()
    }

    fn exists(&self, key: &str) -> bool {
        self.root.join(key).exists()
    }

    fn delete(&self, key: &str) -> std::io::Result<()> {
        std::fs::remove_file(self.root.join(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_store_roundtrip() {
        let dir = std::env::temp_dir().join(format!("nsmt-obj-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = LocalObjectStore::new(&dir);
        store.put("abc123", b"hello").unwrap();
        assert!(store.exists("abc123"));
        assert_eq!(store.get("abc123"), Some(b"hello".to_vec()));
        assert!(!store.exists("nope"));
        store.delete("abc123").unwrap();
        assert!(!store.exists("abc123"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
