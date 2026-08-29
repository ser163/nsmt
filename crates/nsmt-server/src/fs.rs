//! 服务器端共享文件系统（protocol.md §7）。
//!
//! - 对象库：`~/.nsmt/server/<user_domain>/objects/<sha256>`（CAS）
//! - 目录树：`~/.nsmt/server/<user_domain>/trees/<tree_hash>.json` + `latest.json`
//! - 锁：租约锁（内存 + TTL），崩溃可恢复（M2 先用内存）

use nsmt_core::messages::{FileTree, FileTreeEntry};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// 服务器文件存储（按租户隔离）。
#[derive(Clone)]
pub struct ServerFs {
    root: PathBuf,
}

impl ServerFs {
    pub fn new(base: &Path, user_domain: &str) -> Self {
        Self {
            root: base.join("server").join(sanitize(user_domain)),
        }
    }

    fn objects_dir(&self) -> PathBuf {
        self.root.join("objects")
    }
    fn trees_dir(&self) -> PathBuf {
        self.root.join("trees")
    }
    fn latest_path(&self) -> PathBuf {
        self.trees_dir().join("latest.json")
    }

    /// 存一个对象（已存在则跳过，幂等）。
    pub fn put_object(&self, blob_id: &str, bytes: &[u8]) -> std::io::Result<()> {
        let dir = self.objects_dir();
        std::fs::create_dir_all(&dir)?;
        let p = dir.join(blob_id);
        if p.exists() {
            return Ok(());
        }
        std::fs::write(p, bytes)
    }

    pub fn get_object(&self, blob_id: &str) -> Option<Vec<u8>> {
        std::fs::read(self.objects_dir().join(blob_id)).ok()
    }

    pub fn object_exists(&self, blob_id: &str) -> bool {
        self.objects_dir().join(blob_id).exists()
    }

    /// 保存目录树（最新树 + 按 tree_hash 存档）。
    pub fn save_tree(&self, tree: &FileTree) -> std::io::Result<()> {
        std::fs::create_dir_all(self.trees_dir())?;
        let json = serde_json::to_vec_pretty(tree)?;
        std::fs::write(self.trees_dir().join(format!("{}.json", tree.tree_hash)), &json)?;
        std::fs::write(self.latest_path(), json)
    }

    /// 按 tree_hash 读树。
    pub fn get_tree(&self, tree_hash: &str) -> Option<FileTree> {
        let p = self.trees_dir().join(format!("{tree_hash}.json"));
        if !p.exists() {
            return None;
        }
        serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
    }

    pub fn latest_tree(&self) -> Option<FileTree> {
        let p = self.latest_path();
        if !p.exists() {
            return None;
        }
        serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
    }

    /// 计算两个树之间的差异（按路径）。
    pub fn diff(
        old: Option<&FileTree>,
        latest: Option<&FileTree>,
    ) -> (Vec<String>, Vec<String>) {
        let mut old_map: HashMap<String, &FileTreeEntry> = HashMap::new();
        if let Some(o) = old {
            for e in &o.entries {
                old_map.insert(e.path.clone(), e);
            }
        }
        let mut changed = Vec::new();
        let mut removed = Vec::new();
        if let Some(l) = latest {
            for e in &l.entries {
                match old_map.get(&e.path) {
                    None => changed.push(e.path.clone()),
                    Some(o) => {
                        if o.blob_id != e.blob_id || o.mtime_ns != e.mtime_ns {
                            changed.push(e.path.clone());
                        }
                    }
                }
                old_map.remove(&e.path);
            }
        }
        removed.extend(old_map.keys().cloned());
        (changed, removed)
    }
}

/// 目录树 hash（对按路径排序的 JSON 做 SHA-256）。
pub fn tree_hash(tree: &FileTree) -> String {
    let mut entries: Vec<&FileTreeEntry> = tree.entries.iter().collect();
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let mut h = Sha256::new();
    for e in entries {
        h.update(e.path.as_bytes());
        h.update(e.blob_id.as_bytes());
        h.update(e.mode.to_le_bytes());
        h.update(e.size.to_le_bytes());
    }
    hex::encode(h.finalize())
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

/// 租约锁条目。
#[derive(Clone)]
pub struct LockEntry {
    pub holder: String,
    pub expires_at: u64,
}

/// 租约锁注册表。
#[derive(Default)]
pub struct LockRegistry {
    locks: RwLock<HashMap<String, LockEntry>>,
}

impl LockRegistry {
    pub async fn acquire(&self, path: &str, requester: &str, ttl_ms: u64) -> Result<u64, String> {
        let mut g = self.locks.write().await;
        let now = now_ms();
        if let Some(e) = g.get(path) {
            if e.expires_at > now && e.holder != requester {
                return Err(e.holder.clone());
            }
        }
        let expires_at = now + ttl_ms;
        g.insert(
            path.to_string(),
            LockEntry {
                holder: requester.to_string(),
                expires_at,
            },
        );
        Ok(expires_at)
    }

    pub async fn renew(&self, path: &str, requester: &str, ttl_ms: u64) -> bool {
        let mut g = self.locks.write().await;
        match g.get_mut(path) {
            Some(e) if e.holder == requester => {
                e.expires_at = now_ms() + ttl_ms;
                true
            }
            _ => false,
        }
    }

    pub async fn release(&self, path: &str, requester: &str) -> bool {
        let mut g = self.locks.write().await;
        match g.get(path) {
            Some(e) if e.holder == requester => {
                g.remove(path);
                true
            }
            _ => false,
        }
    }

    pub async fn holder(&self, path: &str) -> Option<String> {
        let g = self.locks.read().await;
        g.get(path).map(|e| e.holder.clone())
    }

    /// 定期清理过期锁。
    pub async fn cleanup_loop(self: Arc<Self>) {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(10));
        loop {
            ticker.tick().await;
            let now = now_ms();
            let mut g = self.locks.write().await;
            g.retain(|_, e| e.expires_at > now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn lock_acquire_deny_renew_release() {
        let locks = Arc::new(LockRegistry::default());
        // A 获取锁
        let exp = locks.acquire("docs/plan.md", "maka", 30_000).await.unwrap();
        assert!(exp > now_ms());
        // B 申请同一路径 → 拒绝
        assert_eq!(
            locks.acquire("docs/plan.md", "hermes", 30_000).await.unwrap_err(),
            "maka"
        );
        // 同持有者可续约
        assert!(locks.renew("docs/plan.md", "maka", 30_000).await);
        // 非持有者续约失败
        assert!(!locks.renew("docs/plan.md", "hermes", 30_000).await);
        // 释放后可再获取
        assert!(locks.release("docs/plan.md", "maka").await);
        assert!(locks.acquire("docs/plan.md", "hermes", 30_000).await.is_ok());
    }

    #[tokio::test]
    async fn lock_expires() {
        let locks = Arc::new(LockRegistry::default());
        let _ = locks.acquire("x", "maka", 1).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        // 过期后他人可获取
        assert!(locks.acquire("x", "hermes", 30_000).await.is_ok());
    }

    #[test]
    fn tree_hash_stable_and_sorted() {
        let t1 = FileTree {
            tree_hash: String::new(),
            entries: vec![
                FileTreeEntry { path: "b".into(), blob_id: "1".into(), mode: 0o644, size: 1, mtime_ns: 1 },
                FileTreeEntry { path: "a".into(), blob_id: "2".into(), mode: 0o644, size: 2, mtime_ns: 2 },
            ],
        };
        let t2 = FileTree {
            tree_hash: String::new(),
            entries: vec![
                FileTreeEntry { path: "a".into(), blob_id: "2".into(), mode: 0o644, size: 2, mtime_ns: 2 },
                FileTreeEntry { path: "b".into(), blob_id: "1".into(), mode: 0o644, size: 1, mtime_ns: 1 },
            ],
        };
        // 顺序无关 → 相同 hash
        assert_eq!(tree_hash(&t1), tree_hash(&t2));
    }
}
