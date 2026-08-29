//! 会话处理：每个 QUIC 连接一个任务。
//!
//! 状态机：HELLO → AUTH → REGISTER → ready（心跳 + 记忆/文件/锁帧分发）。
//! M0/M1：AUTH 签名不做强校验（M3 补真实 Ed25519 验证）。

use crate::fs::{tree_hash, ServerFs};
use crate::state::ServerState;
use crate::tenants::TenantStore;
use nsmt_core::frame::{Frame, FrameType};
use nsmt_core::messages::{
    Auth, FileDiff, FileDiffResult, FileGet, FilePut, FileTree, Hello, HelloAck, LockAcquire,
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
    let server_fs = ServerFs::new(
        std::path::Path::new(&std::env::var("NSMT_HOME").unwrap_or_else(|_| {
            std::env::var("HOME").unwrap_or_else(|_| ".".into())
        })),
        &user_domain,
    );

    tracing::info!("machine registered: {} @ {} (agents={})", reg.machine_id, user_domain, reg.agent_tag);

    // ── ready 循环 ──
    let mut heartbeat = tokio::time::interval(crate::registry::HEARTBEAT_INTERVAL);
    heartbeat.tick().await;

    // 文件上传缓冲（FILE_PUT 后跟 FILE_CHUNK）
    let mut pending_put: Option<FilePut> = None;

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
                        if let Some(fp) = pending_put.take() {
                            // 收到对象数据
                            server_fs.put_object(&fp.blob_id, &frame.payload)?;
                            tracing::debug!("object stored: {}", fp.blob_id);
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
                                tree.tree_hash = tree_hash(&tree);
                                server_fs.save_tree(&tree)?;
                                tracing::info!("tree updated: {} entries={}", tree.tree_hash, tree.entries.len());
                            }
                            FrameType::FileDiff => {
                                let diff: FileDiff = frame.payload_json()?;
                                let old = diff.new_tree.as_ref().and_then(|h| server_fs.get_tree(h));
                                let latest = server_fs.latest_tree();
                                let (changed, removed) = ServerFs::diff(old.as_ref(), latest.as_ref());
                                let resp = FileDiffResult { changed, removed, tree: latest };
                                fs.send_json(FrameType::FileDiffResult, 0, &resp).await?;
                            }
                            FrameType::FileGet => {
                                let g: FileGet = frame.payload_json()?;
                                match server_fs.get_object(&g.blob_id) {
                                    Some(data) => {
                                        let c = Frame::new(FrameType::FileChunk, 0, data);
                                        fs.send(&c).await?;
                                    }
                                    None => {
                                        let e = nsmt_core::messages::ErrorMsg {
                                            code: "0xE020".into(),
                                            message: "object not found".into(),
                                            request_id: None,
                                        };
                                        fs.send_json(FrameType::Error, 0, &e).await?;
                                    }
                                }
                            }
                            FrameType::FilePut => {
                                let fp: FilePut = frame.payload_json()?;
                                pending_put = Some(fp);
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
                    machine: MachineInfo { machine_id: reg.machine_id, agents: Vec::new(), addr: String::new(), last_seen: 0 },
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
