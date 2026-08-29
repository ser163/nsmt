//! 会话处理：每个 QUIC 连接一个任务。
//!
//! 状态机：HELLO → AUTH → REGISTER → ready（心跳 + 记忆/文件/锁帧分发）。
//! M0/M1：AUTH 签名不做强校验（M3 补真实 Ed25519 验证）。

use crate::fs::{tree_hash, ServerFs};
use crate::state::ServerState;
use crate::tenants::TenantStore;
use nsmt_core::frame::{Frame, FrameType};
use nsmt_core::messages::{
    Auth, FileDiff, FileDiffResult, FileGet, FilePut, FilePutAck, FileTree, Hello, HelloAck, LockAcquire,
    LockDenied, LockGranted, LockNotify, LockRelease, LockRenew, MachineInfo, MemoryCapture,
    MemoryCaptureResult, MemoryRecall, MemoryRecallResult, OnlineDelta, OnlineDeltaKind, Register,
    RegisterAck,
};
use nsmt_core::FrameStream;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn nonce() -> String {
    use nsmt_core::identity::generate_machine_id;
    format!("nsmt-nonce-{}-{}", now_ms(), generate_machine_id().0)
}

pub async fn handle(conn: quinn::Connection, state: Arc<ServerState>, tenants: Arc<TenantStore>) -> anyhow::Result<()> {
    let (send, recv) = conn.accept_bi().await?;
    let mut fs = FrameStream::new(recv, send);

    // ── HELLO ──
    let frame = fs.recv().await?.ok_or_else(|| anyhow::anyhow!("eof before hello"))?;
    if frame.frame_type != FrameType::Hello {
        return Err(anyhow::anyhow!("expected HELLO, got {:?}", frame.frame_type));
    }
    let hello: Hello = frame.payload_json()?;
    tracing::info!("HELLO from {:?} (client={})", hello.user_domain, hello.client);

    let auth_nonce = nonce();
    fs.send_json(FrameType::HelloAck, 0, &HelloAck { nonce: auth_nonce.clone(), tenant_exists: true })
        .await?;

    // ── AUTH（Ed25519 验签）──
    let frame = fs.recv().await?.ok_or_else(|| anyhow::anyhow!("eof before auth"))?;
    let auth: Auth = frame.payload_json()?;
    if let Err(e) = tenants.verify_auth(&hello.user_domain, &auth_nonce, &auth.nonce_signature).await {
        tracing::warn!("AUTH failed for {}: {e}", hello.user_domain);
        send_error(&mut fs, "0xE002", &format!("auth_failed: {e}"), None).await?;
        return Ok(());
    }

    // ── REGISTER（机器签名校验）──
    let frame = fs.recv().await?.ok_or_else(|| anyhow::anyhow!("eof before register"))?;
    let reg: Register = frame.payload_json()?;
    if let Err(e) = tenants
        .verify_register(&hello.user_domain, &reg.machine_id, &reg.agent_tag, &reg.machine_pubkey, &reg.machine_signature)
        .await
    {
        tracing::warn!("REGISTER failed for {}: {e}", reg.machine_id);
        send_error(&mut fs, "0xE004", &format!("register_failed: {e}"), None).await?;
        return Ok(());
    }

    let user_domain = hello.user_domain.clone();
    let info = MachineInfo {
        machine_id: reg.machine_id.clone(),
        agents: vec![reg.agent_tag.clone()],
        addr: conn.remote_address().to_string(),
        peer_addr: reg.peer_addr.clone(),
        last_seen: now_ms(),
    };

    let snapshot = state.registry.register(&user_domain, info.clone()).await;
    fs.send_json(FrameType::RegisterAck, 0, &RegisterAck { machines: snapshot }).await?;

    let (_, my_tx, mut rx) = state.registry.subscribe(&user_domain).await;
    state
        .registry
        .broadcast_except(
            &user_domain,
            Frame::from_json(
                FrameType::OnlineDelta,
                0,
                &OnlineDelta { kind: OnlineDeltaKind::Join, machine: info },
            )?,
            &my_tx,
        )
        .await;

    // 租户文件存储
    let nsmt_home = std::env::var("NSMT_HOME").unwrap_or_else(|_| {
        std::env::var("HOME").map(|h| format!("{h}/.nsmt")).unwrap_or_else(|_| ".nsmt".into())
    });
    let nsmt_home_path = std::path::PathBuf::from(&nsmt_home);
    let objects = state.object_store_for(&user_domain, &nsmt_home_path).await;
    let server_fs = ServerFs::new(&nsmt_home_path, &user_domain, objects);

    tracing::info!("machine registered: {} @ {} (agents={})", reg.machine_id, user_domain, reg.agent_tag);

    // ── ready 循环 ──
    let mut heartbeat = tokio::time::interval(crate::registry::HEARTBEAT_INTERVAL);
    heartbeat.tick().await;

    // 文件上传缓冲（FILE_PUT → FilePutAck → FILE_CHUNK 分块 → 完成）
    let mut pending_upload: Option<PendingUpload> = None;

    loop {
        tokio::select! {
            _ = heartbeat.tick() => { state.registry.heartbeat(&user_domain, &reg.machine_id).await; }
            ev = rx.recv() => {
                match ev {
                    Some(frame) => { if fs.send(&frame).await.is_err() { break; } }
                    None => break,
                }
            }
            got = fs.recv() => {
                match got {
                    Ok(Some(frame)) => {
                        if frame.frame_type == FrameType::FileChunk {
                            if let Some(pu) = pending_upload.as_mut() {
                                handle_chunk(&server_fs, pu, &frame.payload).await?;
                                if pu.received_count == pu.total_chunks {
                                    if let Some(pu) = pending_upload.take() {
                                        let blob_id = pu.blob_id.clone();
                                        let bytes = std::fs::read(&pu.temp_path)?;
                                        server_fs.put_object(&blob_id, &bytes).await?;
                                        let _ = std::fs::remove_file(&pu.temp_path);
                                        state.record_object_owner(&user_domain, &blob_id, &reg.machine_id).await;
                                        tracing::debug!("object stored (chunked): {}", blob_id);
                                        fs.send_json(FrameType::FilePutAck, 0, &FilePutAck {
                                            blob_id, have: (0..pu.total_chunks).collect(), completed: true,
                                        }).await?;
                                    }
                                }
                            }
                            continue;
                        }
                        match frame.frame_type {
                            FrameType::Heartbeat => {
                                state.registry.heartbeat(&user_domain, &reg.machine_id).await;
                            }
                            FrameType::MemoryRecall => {
                                let msg: MemoryRecall = frame.payload_json()?;
                                match state.pool.recall(&msg).await {
                                    Ok(r) => fs.send_json(FrameType::MemoryRecallResult, 0, &r).await?,
                                    Err(e) => {
                                        tracing::warn!("pool recall failed: {e}");
                                        // 客户端应回退本地托底
                                        let r = MemoryRecallResult {
                                            request_id: msg.request_id,
                                            source: "pool_unavailable".into(),
                                            memories: Vec::new(),
                                            latency_ms: 0,
                                        };
                                        fs.send_json(FrameType::MemoryRecallResult, 0, &r).await?;
                                    }
                                }
                            }
                            FrameType::MemoryCapture => {
                                let msg: MemoryCapture = frame.payload_json()?;
                                match state.pool.capture(&msg).await {
                                    Ok(r) => fs.send_json(FrameType::MemoryCaptureResult, 0, &r).await?,
                                    Err(e) => {
                                        tracing::warn!("pool capture failed: {e}");
                                        fs.send_json(FrameType::MemoryCaptureResult, 0, &MemoryCaptureResult {
                                            request_id: msg.request_id,
                                            committed: false,
                                            queued: true,
                                        }).await?;
                                    }
                                }
                            }
                            FrameType::FileTree => {
                                let mut tree: FileTree = frame.payload_json()?;
                                let client_hash = tree.tree_hash.clone();
                                tree.tree_hash = tree_hash(&tree);
                                tracing::info!("tree recv: client={} recomputed={} entries={}", &client_hash[..12], &tree.tree_hash[..12], tree.entries.len());
                                server_fs.save_tree(&tree)?;
                                tracing::info!("tree updated: {} entries={}", tree.tree_hash, tree.entries.len());
                            }
                            FrameType::FileDiff => {
                                let diff: FileDiff = frame.payload_json()?;
                                let old = server_fs.get_tree(&diff.old_tree);
                                let latest = server_fs.latest_tree();
                                let (changed, removed) = ServerFs::diff(old.as_ref(), latest.as_ref());
                                tracing::info!("diff: old={:?} latest={:?} changed={:?} removed={:?}", old.as_ref().map(|t|&t.tree_hash[..12]), latest.as_ref().map(|t|&t.tree_hash[..12]), changed, removed);
                                let resp = FileDiffResult { changed, removed, tree: latest };
                                fs.send_json(FrameType::FileDiffResult, 0, &resp).await?;
                            }
                            FrameType::FileGet => {
                                let g: FileGet = frame.payload_json()?;
                                match server_fs.get_object(&g.blob_id).await {
                                    Some(data) => {
                                        let idx = g.chunk_index.unwrap_or(0);
                                        let start = (idx * nsmt_core::frame::CHUNK_SIZE as u64) as usize;
                                        let chunk = if start < data.len() {
                                            data[start..std::cmp::min(start + nsmt_core::frame::CHUNK_SIZE, data.len())].to_vec()
                                        } else {
                                            Vec::new()
                                        };
                                        let mut payload = (idx as u32).to_le_bytes().to_vec();
                                        let cipher = nsmt_core::e2e::e2e_key_from_env();
                                        let enc = nsmt_core::e2e::encrypt_payload(cipher.as_ref(), &chunk, idx)?;
                                        payload.extend_from_slice(&enc);
                                        let mut c = Frame::new(FrameType::FileChunk, 0, payload);
                                        if cipher.is_some() {
                                            c.flags = nsmt_core::frame::Flags(0x01);
                                        }
                                        fs.send(&c).await?;
                                    }
                                    None => {
                                        // 服务器没有 → 尝试返回持有者 peer 地址（P2P 直连拉取）
                                        let peer_hint = match state.object_owner(&user_domain, &g.blob_id).await {
                                            Some(owner) => state.registry.online_machine(&user_domain, &owner).await
                                                .map(|m| m.peer_addr).unwrap_or_default(),
                                            None => String::new(),
                                        };
                                        let e = nsmt_core::messages::ErrorMsg {
                                            code: "0xE020".into(),
                                            message: if peer_hint.is_empty() { "object not found".into() } else { format!("object not found; peer={peer_hint}") },
                                            request_id: None,
                                        };
                                        fs.send_json(FrameType::Error, 0, &e).await?;
                                    }
                                }
                            }
                            FrameType::FilePut => {
                                let fp: FilePut = frame.payload_json()?;
                                // 多租户配额：预占（超限拒绝）
                                if !state.try_reserve_quota(&user_domain, fp.size).await {
                                    tracing::warn!("quota exceeded for tenant {user_domain} (size={})", fp.size);
                                    let e = nsmt_core::messages::ErrorMsg { code: "0xE040".into(), message: "quota_exceeded".into(), request_id: None };
                                    fs.send_json(FrameType::Error, 0, &e).await?;
                                    continue;
                                }
                                let temp_path = server_fs.temp_path_for(&fp.blob_id);
                                let _ = std::fs::remove_file(&temp_path);
                                pending_upload = Some(PendingUpload {
                                    blob_id: fp.blob_id.clone(),
                                    size: fp.size,
                                    total_chunks: fp.total_chunks,
                                    temp_path,
                                    received: vec![false; fp.total_chunks as usize],
                                    received_count: 0,
                                });
                                fs.send_json(FrameType::FilePutAck, 0, &FilePutAck {
                                    blob_id: fp.blob_id, have: Vec::new(), completed: false,
                                }).await?;
                            }
                            FrameType::LockAcquire => {
                                let l: LockAcquire = frame.payload_json()?;
                                match state.locks.acquire(&l.path, &l.requester, l.ttl_ms).await {
                                    Ok(exp) => {
                                        fs.send_json(FrameType::LockGranted, 0, &LockGranted { path: l.path.clone(), expires_at: exp }).await?;
                                        state.registry.broadcast_except(&user_domain, Frame::from_json(FrameType::LockNotify, 0, &LockNotify { path: l.path, event: "locked".into(), holder: Some(l.requester.clone()) })?, &my_tx).await;
                                    }
                                    Err(holder) => {
                                        fs.send_json(FrameType::LockDenied, 0, &LockDenied { path: l.path, holder }).await?;
                                    }
                                }
                            }
                            FrameType::LockRenew => {
                                let l: LockRenew = frame.payload_json()?;
                                let ok = state.locks.renew(&l.path, &l.requester, 30_000).await;
                                fs.send_json(FrameType::LockGranted, 0, &LockGranted { path: l.path, expires_at: now_ms() + 30_000 }).await?;
                                let _ = ok;
                            }
                            FrameType::LockRelease => {
                                let l: LockRelease = frame.payload_json()?;
                                state.locks.release(&l.path, &l.requester).await;
                                state.registry.broadcast_except(&user_domain, Frame::from_json(FrameType::LockNotify, 0, &LockNotify { path: l.path, event: "unlocked".into(), holder: None })?, &my_tx).await;
                            }
                            _ => { tracing::debug!("unhandled frame {:?}", frame.frame_type); }
                        }
                    }
                    Ok(None) => break,
                    Err(e) => { tracing::debug!("recv error: {e}"); break; }
                }
            }
        }
    }

    state
        .registry
        .broadcast(
            &user_domain,
            Frame::from_json(
                FrameType::OnlineDelta,
                0,
                &OnlineDelta {
                    kind: OnlineDeltaKind::Leave,
                    machine: MachineInfo { machine_id: reg.machine_id, agents: Vec::new(), addr: String::new(), peer_addr: String::new(), last_seen: 0 },
                },
            )?,
        )
        .await;

    Ok(())
}


async fn send_error<R, W>(fs: &mut FrameStream<R, W>, code: &str, message: &str, request_id: Option<String>) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    fs.send_json(
        FrameType::Error,
        0,
        &nsmt_core::messages::ErrorMsg {
            code: code.into(),
            message: message.into(),
            request_id,
        },
    )
    .await?;
    Ok(())
}

/// 进行中的分块上传。
struct PendingUpload {
    blob_id: String,
    size: u64,
    total_chunks: u64,
    temp_path: std::path::PathBuf,
    received: Vec<bool>,
    received_count: u64,
}

/// 处理一个 FILE_CHUNK 载荷：`[u32 LE chunk_index][data]`。
async fn handle_chunk(
    server_fs: &crate::fs::ServerFs,
    pu: &mut PendingUpload,
    payload: &[u8],
) -> anyhow::Result<()> {
    if payload.len() < 4 {
        return Ok(());
    }
    let idx = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as u64;
    if idx >= pu.total_chunks || pu.received[idx as usize] {
        return Ok(());
    }
    let cipher = nsmt_core::e2e::e2e_key_from_env();
    let data = nsmt_core::e2e::decrypt_payload(cipher.as_ref(), &payload[4..], idx)?;
    std::fs::create_dir_all(pu.temp_path.parent().unwrap_or(std::path::Path::new(".")))?;
    let offset = (idx * nsmt_core::frame::CHUNK_SIZE as u64) as u64;
    use std::io::{Seek, SeekFrom, Write};
    let mut f = std::fs::OpenOptions::new().create(true).write(true).open(&pu.temp_path)?;
    f.seek(SeekFrom::Start(offset))?;
    f.write_all(&data)?;
    pu.received[idx as usize] = true;
    pu.received_count += 1;
    Ok(())
}
