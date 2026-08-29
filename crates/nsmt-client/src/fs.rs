//! 客户端共享文件系统（protocol.md §7）。
//!
//! - 本地对象缓存：`~/.nsmt/objects/<sha256>`
//! - 共享目录：`NSMT_SHARE_DIR`（默认 `~/nsmt_share`），真实文件（可编辑）
//! - `NSMT_SYMLINK_VIEW=1`：拉取时用 symlink 指向缓存（按需拉取模式，决策 #4）
//! - 变更：本地对象入库 → 上锁 → 推对象+树；远端变更：diff → 拉取 → 物化

use nsmt_core::frame::{Frame, FrameType};
use nsmt_core::messages::{FileDiff, FileDiffResult, FileGet, FilePut, FilePutAck, FileTree, FileTreeEntry, LockAcquire, LockRelease, PeerHint};
use nsmt_core::FrameStream;
use sha2::{Digest, Sha256};
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// M9.1 NAT 打洞钩子：收到服务器 PeerHint（有人在请求本机对象）时，由 main 注册
/// 一个回调主动向 requester 地址打洞（打开 NAT 映射），之后 requester 直连才通。
pub static PEER_HINT_HOOK: std::sync::OnceLock<
    Box<dyn Fn(&str, &str) + Send + Sync>,
> = std::sync::OnceLock::new();

pub fn maybe_peer_hint(frame: &Frame) {
    if frame.frame_type != FrameType::PeerHint {
        return;
    }
    if let Ok(h) = frame.payload_json::<PeerHint>() {
        if let Some(hook) = PEER_HINT_HOOK.get() {
            hook(&h.blob_id, &h.requester_addr);
        }
    }
}

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
            let rel = p
                .strip_prefix(root)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/");
            // 冲突副本 / 本地缓存标记 不参与同步
            if rel.starts_with(".sync-conflict-") || rel.starts_with(".nsmt-") {
                continue;
            }
            let bytes = std::fs::read(&p)?;
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

/// 物化时处理冲突：本地已有且内容不同 → 保留冲突副本（.sync-conflict），再写入远端版。
pub fn materialize_with_conflict(entry: &FileTreeEntry, bytes: &[u8], requester: &str) -> std::io::Result<()> {
    let dir = share_dir();
    let target = dir.join(&entry.path);
    if target.exists() {
        if let Ok(local_bytes) = std::fs::read(&target) {
            if object_id(&local_bytes) != entry.blob_id {
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                let conflict = target.with_file_name(format!(
                    ".sync-conflict-{}-{}-{ts}",
                    sanitize_name(requester),
                    entry.path.replace('/', "_")
                ));
                let _ = std::fs::copy(&target, &conflict);
                tracing::warn!("conflict: local {} differs -> kept {}", entry.path, conflict.display());
            }
        }
    }
    materialize(entry, bytes)
}

fn sanitize_name(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
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
        std::fs::read(share_dir().join(&entry.path))?
    };

    // 分块上传（断点续传：按 FilePutAck.have 只传缺失块）
    let total_chunks = bytes.len().div_ceil(nsmt_core::frame::CHUNK_SIZE) as u64;
    fs.send_json(FrameType::FilePut, 0, &FilePut {
        blob_id: entry.blob_id.clone(),
        total_chunks,
        size: bytes.len() as u64,
    }).await?;
    // 读初始 ack（已拥有块列表）；配额/错误时服务器回 Error → 中止
    let ack_frame = loop {
        let f = fs.recv().await?.ok_or_else(|| anyhow::anyhow!("eof"))?;
        if f.frame_type == FrameType::FilePutAck || f.frame_type == FrameType::Error {
            break f;
        }
    };
    if ack_frame.frame_type == FrameType::Error {
        let e: nsmt_core::messages::ErrorMsg = ack_frame.payload_json()?;
        tracing::warn!("upload rejected: {} ({})", e.message, e.code);
        return Ok(());
    }
    let ack: FilePutAck = ack_frame.payload_json()?;
    for idx in 0..total_chunks {
        if ack.have.contains(&idx) {
            continue; // 已传过（续传）
        }
        let start = (idx * nsmt_core::frame::CHUNK_SIZE as u64) as usize;
        let end = std::cmp::min(start + nsmt_core::frame::CHUNK_SIZE, bytes.len());
        let mut payload = (idx as u32).to_le_bytes().to_vec();
        // M9.4：按租户密钥加密（domain = requester FQN 前缀），支持轮换
        let domain = requester.split('/').next().unwrap_or("");
        let e2e = nsmt_core::e2e::E2EKeys::from_env(if domain.is_empty() { None } else { Some(domain) });
        let enc = match &e2e {
            Some(k) => k.encrypt(&bytes[start..end], idx)?,
            None => bytes[start..end].to_vec(),
        };
        payload.extend_from_slice(&enc);
        let mut f = Frame::new(FrameType::FileChunk, 0, payload);
        if e2e.is_some() {
            f.flags = nsmt_core::frame::Flags(0x01); // bit0: E2E 加密
        }
        fs.send(&f).await?;
    }
    // 读完成 ack
    let done: FilePutAck = read_until(fs, FrameType::FilePutAck).await?.payload_json()?;
    if !done.completed {
        tracing::warn!("upload not completed for {}", entry.blob_id);
    }

    // 释放锁
    fs.send_json(FrameType::LockRelease, 0, &LockRelease { path: entry.path.clone(), requester: requester.to_string() }).await?;
    Ok(())
}

/// 循环读帧直到出现目标类型（跳过广播帧）。
async fn read_until<R, W>(
    fs: &mut FrameStream<R, W>,
    want: FrameType,
) -> anyhow::Result<Frame>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    loop {
        let f = fs.recv().await?.ok_or_else(|| anyhow::anyhow!("eof"))?;
        maybe_peer_hint(&f);
        if f.frame_type == want {
            return Ok(f);
        }
    }
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
        maybe_peer_hint(&resp);
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

/// 分块拉取对象（FILE_GET → FILE_CHUNK，支持断点续传：失败后从已收块继续）。
/// `domain` 用于派生租户 E2E 密钥（M9.4）。
pub async fn get_object<R, W>(fs: &mut FrameStream<R, W>, blob_id: &str, domain: &str) -> anyhow::Result<Option<Vec<u8>>>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut out = Vec::new();
    let mut idx: u64 = 0;
    loop {
        fs.send_json(FrameType::FileGet, 0, &FileGet { blob_id: blob_id.to_string(), chunk_index: Some(idx) }).await?;
        let resp = loop {
            let f = fs.recv().await?.ok_or_else(|| anyhow::anyhow!("eof"))?;
            maybe_peer_hint(&f);
            if f.frame_type == FrameType::FileChunk || f.frame_type == FrameType::Error {
                break f;
            }
        };
        match resp.frame_type {
            FrameType::Error => return Ok(None),
            FrameType::FileChunk => {
                if resp.payload.len() < 4 {
                    return Ok(Some(out));
                }
                let got_idx = u32::from_le_bytes([resp.payload[0], resp.payload[1], resp.payload[2], resp.payload[3]]) as u64;
                if got_idx != idx {
                    continue;
                }
                // M9.4：按租户密钥解密（轮换：尝试全部密钥）
                let data = match nsmt_core::e2e::E2EKeys::from_env(if domain.is_empty() { None } else { Some(domain) }) {
                    Some(k) => k.decrypt(&resp.payload[4..], idx)?,
                    None => resp.payload[4..].to_vec(),
                };
                out.extend_from_slice(&data);
                if data.len() < nsmt_core::frame::CHUNK_SIZE {
                    return Ok(Some(out)); // 最后一块
                }
                idx += 1;
            }
            _ => unreachable!(),
        }
    }
}
