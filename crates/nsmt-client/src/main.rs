//! NSMT 客户端 `yggd` — M0：QUIC 连接 + HELLO/AUTH/REGISTER + 心跳 + 在线列表。

use anyhow::Context;
use nsmt_core::frame::FrameType;
use nsmt_core::identity::{generate_machine_id, AgentTag};
use nsmt_core::messages::{
    Auth, Heartbeat, Hello, HelloAck, MachineInfo, OnlineDelta, OnlineList, Register, RegisterAck,
};
use nsmt_core::FrameStream;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let server_addr: SocketAddr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:5555".into())
        .parse()
        .context("usage: yggd [server_addr]")?;
    let user_domain = std::env::var("NSMT_USER_DOMAIN").unwrap_or_else(|_| "ser163".into());
    let agent = std::env::var("NSMT_AGENT_TAG").unwrap_or_else(|_| "maka".into());
    let _agent_tag = AgentTag::new(agent.clone()).context("invalid agent tag")?;

    // ── TLS（信任服务器证书 ~/.nsmt/ygg.crt）──
    let cert_path = std::env::var("NSMT_SERVER_CERT").unwrap_or_else(|_| {
        std::env::var("HOME").map(|h| format!("{h}/.nsmt/ygg.crt")).unwrap_or_else(|_| ".nsmt/ygg.crt".into())
    });
    let cert_pem = std::fs::read(&cert_path)
        .with_context(|| format!("读取服务器证书失败：{cert_path}（先启动 ygg 生成）"))?;
    let cert_der = rustls::pki_types::CertificateDer::from(cert_pem);
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert_der).context("add root cert")?;
    let rustls_client = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let client_config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(rustls_client)?,
    ));

    // ── 连接 ──
    let endpoint = quinn::Endpoint::client("0.0.0.0:0".parse()?)?;
    let conn = endpoint
        .connect_with(client_config, server_addr, "localhost")
        .context("connect")?
        .await
        .context("quinn handshake")?;
    tracing::info!("QUIC connected to {server_addr}");

    let (send, recv) = conn.open_bi().await?;
    let mut fs = FrameStream::new(recv, send);

    // ── HELLO ──
    fs.send_json(
        FrameType::Hello,
        0,
        &Hello {
            user_domain: user_domain.clone(),
            protocol_version: nsmt_core::frame::PROTOCOL_VERSION,
            client: format!("yggd/{}", env!("CARGO_PKG_VERSION")),
        },
    )
    .await?;
    let ack: HelloAck = fs
        .recv()
        .await?
        .ok_or_else(|| anyhow::anyhow!("eof"))?
        .payload_json()?;
    tracing::info!("HELLO_ACK nonce={}", ack.nonce);

    // ── AUTH（M0 占位签名）──
    fs.send_json(
        FrameType::Auth,
        0,
        &Auth {
            user_domain: user_domain.clone(),
            nonce_signature: "M0-placeholder-signature".into(),
        },
    )
    .await?;

    // ── REGISTER（NSMT_MACHINE_ID 可覆盖，便于测试多机）──
    let (hw_machine_id, _stable) = generate_machine_id();
    let machine_id = std::env::var("NSMT_MACHINE_ID").unwrap_or_else(|_| hw_machine_id.to_string());
    fs.send_json(
        FrameType::Register,
        0,
        &Register {
            machine_id: machine_id.to_string(),
            agent_tag: agent.clone(),
            machine_signature: "M0-placeholder".into(),
        },
    )
    .await?;
    let ack: RegisterAck = fs
        .recv()
        .await?
        .ok_or_else(|| anyhow::anyhow!("eof"))?
        .payload_json()?;
    tracing::info!(
        "registered: {}  online machines: {}",
        machine_id,
        ack.machines.len()
    );
    print_online(&ack.machines);

    // ── 心跳 + 广播接收 ──
    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(10));
    heartbeat.tick().await;
    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                fs.send_json(FrameType::Heartbeat, 0, &Heartbeat { ts: now_ms(), load: None }).await?;
            }
            got = fs.recv() => {
                match got {
                    Ok(Some(f)) => match f.frame_type {
                        FrameType::OnlineList => {
                            let m: OnlineList = f.payload_json()?;
                            print_online(&m.machines);
                        }
                        FrameType::OnlineDelta => {
                            let d: OnlineDelta = f.payload_json()?;
                            println!("[online-delta] {:?} {}", d.kind, d.machine.machine_id);
                        }
                        _ => { /* M1 处理记忆/文件/锁 */ }
                    },
                    Ok(None) => { tracing::info!("server closed connection"); break; }
                    Err(e) => { tracing::error!("recv error: {e}"); break; }
                }
            }
        }
    }
    Ok(())
}

fn print_online(machines: &[MachineInfo]) {
    println!("=== online machines ({}) ===", machines.len());
    for m in machines {
        println!("  {}  agents={:?}  last_seen={}", m.machine_id, m.agents, m.last_seen);
    }
}

