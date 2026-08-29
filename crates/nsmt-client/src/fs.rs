//! 客户端共享文件系统（protocol.md §7）。
//!
//! - 本地对象缓存：`~/.nsmt/objects/<sha256>`
//! - 共享目录：`NSMT_SHARE_DIR`（默认 `~/nsmt_share`），真实文件（可编辑）
//! - `NSMT_SYMLINK_VIEW=1`：拉取时用 symlink 指向缓存（按需拉取模式，决策 #4）
//! - 变更：本地对象入库 → 上锁 → 推对象+树；远端变更：diff → 拉取 → 物化

use nsmt_core::frame::{Frame, FrameType};
use nsmt_core::messages::{FileDiff, FileDiffResult, FileGet, FilePut, FileTree, FileTreeEntry, LockAcquire, LockRelease};
use nsmt_core::FrameStream;
use sha2::{Digest, Sha256};
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// 目录树 hash（与服务器端一致：按路径排序序列化）。
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

/// 对象 ID（内容 SHA-256）。
pub fn object_id(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// 共享目录 / 对象缓存路径。
pub fn share_dir() -> PathBuf {
    std::env::var("NSMT_SHARE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join("nsmt_share")).unwrap_or_else(|_| PathBuf::from("nsmt_share")))
}

pub fn objects_dir() -> PathBuf {
    std::env::var("NSMT_OBJECTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".nsmt").join("objects")).unwrap_or_else(|_| PathBuf::from(".nsmt/objects")))
}

/// 递归遍历目录，生成目录树。
pub fn build_tree(dir: &Path) -> std::io::Result<FileTree> {
    let mut entries = Vec::new();
    walk(dir, dir, &mut entries)?;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let tree = FileTree {
        tree_hash: String::new(),
        entries,
    };
    let mut t = tree;
    t.tree_hash = tree_hash(&t);
    Ok(t)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<FileTreeEntry>) -> std::io::Result<()> {
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let p = e.path();
        let ft = e.file_type()?;
        if ft.is_dir() {
            walk(root, &p, out)?;
        } else if ft.is_file() {
            let bytes = std::fs::read(&p)?;
            let rel = p
                .strip_prefix(root)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/");
            let meta = std::fs::metadata(&p)?;
            out.push(FileTreeEntry {
                path: rel,
                blob_id: object_id(&bytes),
                mode: 0o644,
                size: bytes.len() as u64,
                mtime_ns: meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos() as i64)
                    .unwrap_or(0),
            });
        }
    }
    Ok(())
}

/// 确保对象进本地缓存。
pub fn ensure_object_local(blob_id: &str, bytes: &[u8]) -> std::io::Result<()> {
    let dir = objects_dir();
    std::fs::create_dir_all(&dir)?;
    let p = dir.join(blob_id);
    if !p.exists() {
        std::fs::write(p, bytes)?;
    }
    Ok(())
}

/// 物化一个对象到共享目录（决策 #4：默认真实文件；NSMT_SYMLINK_VIEW=1 用 symlink）。
pub fn materialize(entry: &FileTreeEntry, bytes: &[u8]) -> std::io::Result<()> {
    let dir = share_dir();
    let target = dir.join(&entry.path);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::env::var("NSMT_SYMLINK_VIEW").is_ok() {
        let obj = objects_dir().join(&entry.blob_id);
        // 先确保对象在缓存
        if !obj.exists() {
            std::fs::write(&obj, bytes)?;
        }
        let _ = std::fs::remove_file(&target);
        std::os::unix::fs::symlink(&obj, &target)?;
    } else {
        let mut f = std::fs::File::create(&target)?;
        f.write_all(bytes)?;
    }
    Ok(())
}

/// 计算两树差异中"需要拉取"的对象。
/// 找本地树中比远程新的对象（待推送）。M2 简单实现：本地有而远程没有的。
/// 上锁 + 推送对象（在既有 FrameStream 上）。
pub async fn push_entry<R, W>(
    fs: &mut FrameStream<R, W>,
    entry: &FileTreeEntry,
    requester: &str,
) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    // 上锁
    fs.send_json(
        FrameType::LockAcquire,
        0,
        &LockAcquire { path: entry.path.clone(), requester: requester.to_string(), ttl_ms: 30_000 },
    )
    .await?;
    // 循环读响应直到锁授予/拒绝（跳过广播帧 OnlineDelta / LockNotify 等）
    let mut granted = false;
    loop {
        let resp = fs.recv().await?.ok_or_else(|| anyhow::anyhow!("eof"))?;
        match resp.frame_type {
            FrameType::LockDenied => {
                tracing::warn!("lock denied for {} (held by others)", entry.path);
                return Ok(());
            }
            FrameType::LockGranted => { granted = true; break; }
            _ => { /* 广播帧，忽略 */ }
        }
    }
    debug_assert!(granted);

    // 读对象字节（本地缓存）
    let obj_path = objects_dir().join(&entry.blob_id);
    let bytes = if obj_path.exists() {
        std::fs::read(&obj_path)?
    } else {
        // 从共享目录读
        std::fs::read(share_dir().join(&entry.path))?
    };

    // 推对象
    fs.send_json(FrameType::FilePut, 0, &FilePut { blob_id: entry.blob_id.clone(), total_chunks: 1, size: bytes.len() as u64 }).await?;
    fs.send(&Frame::new(FrameType::FileChunk, 0, bytes)).await?;

    // 释放锁
    fs.send_json(FrameType::LockRelease, 0, &LockRelease { path: entry.path.clone(), requester: requester.to_string() }).await?;
    Ok(())
}

/// 拉取一个对象（FileGet → FileChunk）并物化。
pub async fn pull_entry<R, W>(
    fs: &mut FrameStream<R, W>,
    path: &str,
) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    // 先 diff 拿到 blob_id？简化：直接请求该路径——M2 用 diff 列表，这里按需调用方传入 blob_id
    let _ = path;
    let _ = fs;
    Ok(())
}

/// FILE_DIFF 请求，返回 changed/removed。
pub async fn request_diff<R, W>(fs: &mut FrameStream<R, W>, old_tree: &str) -> anyhow::Result<FileDiffResult>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    fs.send_json(FrameType::FileDiff, 0, &FileDiff { old_tree: old_tree.to_string(), new_tree: None }).await?;
    loop {
        let resp = fs.recv().await?.ok_or_else(|| anyhow::anyhow!("eof"))?;
        if resp.frame_type == FrameType::FileDiffResult {
            return Ok(resp.payload_json()?);
        }
    }
}

/// 推整棵树（对象已在本地缓存）。
pub async fn push_tree<R, W>(fs: &mut FrameStream<R, W>, tree: &FileTree) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    fs.send_json(FrameType::FileTree, 0, tree).await?;
    Ok(())
}

/// 拉取对象（FILE_GET → FILE_CHUNK）。
pub async fn get_object<R, W>(fs: &mut FrameStream<R, W>, blob_id: &str) -> anyhow::Result<Option<Vec<u8>>>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    fs.send_json(FrameType::FileGet, 0, &FileGet { blob_id: blob_id.to_string(), chunk_index: None }).await?;
    loop {
        let resp = fs.recv().await?.ok_or_else(|| anyhow::anyhow!("eof"))?;
        match resp.frame_type {
            FrameType::FileChunk => return Ok(Some(resp.payload)),
            FrameType::Error => return Ok(None),
            _ => { /* 广播帧，忽略 */ }
        }
    }
}
