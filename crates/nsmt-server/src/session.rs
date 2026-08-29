//! 会话处理：每个 QUIC 连接一个任务。
//!
//! 状态机：HELLO → AUTH → REGISTER → ready（心跳 + 订阅广播）。
//! M0 开发期：AUTH 签名不做强校验（M1/M3 补真实 Ed25519 验证）。

use crate::registry::Registry;
use nsmt_core::frame::{Frame, FrameType};
use nsmt_core::messages::{
    Auth, Hello, HelloAck, MachineInfo, OnlineDelta, OnlineDeltaKind, Register, RegisterAck,
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

pub async fn handle(conn: quinn::Connection, registry: Arc<Registry>) -> anyhow::Result<()> {
    let (send, recv) = conn.accept_bi().await?;
    let mut fs = FrameStream::new(recv, send);

    // ── HELLO ──
    let frame = fs.recv().await?.ok_or_else(|| anyhow::anyhow!("eof before hello"))?;
    if frame.frame_type != FrameType::Hello {
        return Err(anyhow::anyhow!("expected HELLO, got {:?}", frame.frame_type));
    }
    let hello: Hello = frame.payload_json()?;
    tracing::info!("HELLO from {:?} (client={})", hello.user_domain, hello.client);

    // ── HELLO_ACK ──
    let ack = HelloAck {
        nonce: nonce(),
        tenant_exists: true,
    };
    fs.send_json(FrameType::HelloAck, 0, &ack).await?;

    // ── AUTH（M0 不强校验签名）──
    let frame = fs.recv().await?.ok_or_else(|| anyhow::anyhow!("eof before auth"))?;
    let _auth: Auth = frame.payload_json()?;

    // ── REGISTER ──
    let frame = fs.recv().await?.ok_or_else(|| anyhow::anyhow!("eof before register"))?;
    let reg: Register = frame.payload_json()?;

    let user_domain = hello.user_domain.clone();
    let info = MachineInfo {
        machine_id: reg.machine_id.clone(),
        agents: vec![reg.agent_tag.clone()],
        addr: conn.remote_address().to_string(),
        last_seen: now_ms(),
    };

    let snapshot = registry.register(&user_domain, info.clone()).await;
    let ack = RegisterAck { machines: snapshot };
    fs.send_json(FrameType::RegisterAck, 0, &ack).await?;

    // 订阅租户广播
    let (_, mut rx) = registry.subscribe(&user_domain).await;

    // 广播 join
    registry
        .broadcast(
            &user_domain,
            Frame::from_json(
                FrameType::OnlineDelta,
                0,
                &OnlineDelta {
                    kind: OnlineDeltaKind::Join,
                    machine: info,
                },
            )?,
        )
        .await;

    tracing::info!(
        "machine registered: {} @ {} (agents={:?})",
        reg.machine_id,
        user_domain,
        reg.agent_tag
    );

    // ── ready：心跳 + 订阅广播循环 ──
    let mut heartbeat = tokio::time::interval(crate::registry::HEARTBEAT_INTERVAL);
    heartbeat.tick().await; // 首次立即

    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                registry.heartbeat(&user_domain, &reg.machine_id).await;
            }
            ev = rx.recv() => {
                match ev {
                    Some(frame) => {
                        if fs.send(&frame).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
            got = fs.recv() => {
                match got {
                    Ok(Some(f)) => {
                        match f.frame_type {
                            FrameType::Heartbeat => {
                                registry.heartbeat(&user_domain, &reg.machine_id).await;
                            }
                            _ => {
                                // M1 起处理记忆/文件/锁帧
                                tracing::debug!("unhandled frame {:?} in M0", f.frame_type);
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        tracing::debug!("recv error: {e}");
                        break;
                    }
                }
            }
        }
    }

    registry
        .broadcast(
            &user_domain,
            Frame::from_json(
                FrameType::OnlineDelta,
                0,
                &OnlineDelta {
                    kind: OnlineDeltaKind::Leave,
                    machine: MachineInfo {
                        machine_id: reg.machine_id,
                        agents: Vec::new(),
                        addr: String::new(),
                        last_seen: 0,
                    },
                },
            )?,
        )
        .await;

    Ok(())
}
