//! NSMT 客户端 `yggd`。
//!
//! 用法：
//!   yggd <server>                         在线模式（心跳 + 在线列表 + 文件同步）
//!   yggd <server> capture <user> <assistant>   记忆双写（域池+本地托底）
//!   yggd <server> recall <query>               记忆召回（网络优先，超时回退本地托底）
//!
//! 环境变量：NSMT_USER_DOMAIN / NSMT_AGENT_TAG / NSMT_MACHINE_ID（测试覆盖）/
//!           NSMT_SHARE_DIR / NSMT_OBJECTS_DIR / NSMT_SYMLINK_VIEW

mod fs;
mod memory;

use anyhow::Context;
use nsmt_core::frame::FrameType;
use notify::Watcher as _;
use nsmt_core::identity::{generate_machine_id, AgentTag};
use nsmt_core::messages::{
    Auth, FileTree, Heartbeat, Hello, HelloAck, MachineInfo, MemoryCapture, MemoryRecall,
    OnlineDelta, OnlineList, Register, RegisterAck,
};
use nsmt_core::FrameStream;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let args: Vec<String> = std::env::args().collect();
    let server_addr: SocketAddr = args
        .get(1)
        .map(|s| s.parse())
        .unwrap_or_else(|| "127.0.0.1:5555".parse())
        .context("usage: yggd <server> [capture|recall|fs] ...")?;
    let user_domain = std::env::var("NSMT_USER_DOMAIN").unwrap_or_else(|_| "ser163".into());
    let agent = std::env::var("NSMT_AGENT_TAG").unwrap_or_else(|_| "maka".into());
    AgentTag::new(agent.clone()).context("invalid agent tag")?;
    let (hw_machine, _) = generate_machine_id();
    let machine_id = std::env::var("NSMT_MACHINE_ID").unwrap_or_else(|_| hw_machine.to_string());

    let fallback = memory::LocalFallback::from_env();
    let fqn = format!("{user_domain}/{machine_id}/{agent}");

    let command = args.get(2).map(|s| s.as_str());

    let conn = connect(server_addr).await?;
    tracing::info!("QUIC connected to {server_addr}");
    let (send, recv) = conn.open_bi().await?;
    let mut fs = FrameStream::new(recv, send);
    handshake(&mut fs, &user_domain, &agent, &machine_id).await?;

    match command {
        Some("capture") => {
            let user = args.get(3).cloned().unwrap_or_else(|| "（无输入）".into());
            let assistant = args.get(4).cloned().unwrap_or_else(|| "（无回复）".into());
            capture(&mut fs, &fallback, &fqn, &user, &assistant).await?;
        }
        Some("recall") => {
            let query = args.get(3).cloned().unwrap_or_else(|| "".into());
            recall(&mut fs, &fallback, &query).await?;
        }
        Some("fs") => {
            fs_mode(&mut fs, &fqn).await?;
        }
        _ => {
            online_mode(&mut fs, &fqn).await?;
        }
    }
    Ok(())
}

async fn connect(addr: SocketAddr) -> anyhow::Result<quinn::Connection> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
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
    let endpoint = quinn::Endpoint::client("0.0.0.0:0".parse()?)?;
    Ok(endpoint.connect_with(client_config, addr, "localhost").context("connect")?.await.context("handshake")?)
}

async fn handshake<R, W>(fs: &mut FrameStream<R, W>, domain: &str, agent: &str, machine: &str) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    fs.send_json(FrameType::Hello, 0, &Hello {
        user_domain: domain.into(),
        protocol_version: nsmt_core::frame::PROTOCOL_VERSION,
        client: format!("yggd/{}", env!("CARGO_PKG_VERSION")),
    }).await?;
    let _ack: HelloAck = fs.recv().await?.ok_or_else(|| anyhow::anyhow!("eof"))?.payload_json()?;
    fs.send_json(FrameType::Auth, 0, &Auth {
        user_domain: domain.into(),
        nonce_signature: "M0-placeholder".into(),
    }).await?;
    fs.send_json(FrameType::Register, 0, &Register {
        machine_id: machine.into(),
        agent_tag: agent.into(),
        machine_signature: "M0-placeholder".into(),
    }).await?;
    let ack: RegisterAck = fs.recv().await?.ok_or_else(|| anyhow::anyhow!("eof"))?.payload_json()?;
    tracing::info!("registered {machine} online={}", ack.machines.len());
    Ok(())
}

// ── M1：记忆 ──

async fn capture<R, W>(fs: &mut FrameStream<R, W>, fallback: &memory::LocalFallback, fqn: &str, user: &str, assistant: &str) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let req = MemoryCapture {
        request_id: format!("cap-{}", now_ms()),
        user_content: user.into(),
        assistant_content: assistant.into(),
        scope: "user".into(),
        fqn: fqn.into(),
        observed_at: now_ms(),
    };
    // 双写①：域池（经服务器）；循环读帧直到 CaptureResult
    fs.send_json(FrameType::MemoryCapture, 0, &req).await?;
    let r = loop {
        let resp = fs.recv().await?.ok_or_else(|| anyhow::anyhow!("eof"))?;
        match resp.frame_type {
            FrameType::MemoryCaptureResult => break resp.payload_json::<nsmt_core::messages::MemoryCaptureResult>()?,
            FrameType::OnlineDelta => { let d: OnlineDelta = resp.payload_json()?; println!("[online-delta] {:?} {}", d.kind, d.machine.machine_id); }
            FrameType::OnlineList => { let m: OnlineList = resp.payload_json()?; print_online(&m.machines); }
            _ => {}
        }
    };
    // 双写②：本地托底
    match fallback.capture_local(user, assistant, fqn).await {
        Ok(()) => tracing::info!("local fallback capture OK"),
        Err(e) => tracing::warn!("local fallback capture failed: {e}"),
    }
    println!("capture: pool_committed={} local_written=yes fqn={fqn}", r.committed);
    Ok(())
}

async fn recall<R, W>(fs: &mut FrameStream<R, W>, fallback: &memory::LocalFallback, query: &str) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let req = MemoryRecall {
        request_id: format!("rec-{}", now_ms()),
        query: query.into(),
        scope: "user".into(),
        limit: 5,
        timeout_ms: 1500,
        fallback_on_timeout: true,
    };
    fs.send_json(FrameType::MemoryRecall, 0, &req).await?;
    let r = loop {
        let resp = fs.recv().await?.ok_or_else(|| anyhow::anyhow!("eof"))?;
        match resp.frame_type {
            FrameType::MemoryRecallResult => break resp.payload_json::<nsmt_core::messages::MemoryRecallResult>()?,
            FrameType::OnlineDelta => { let d: OnlineDelta = resp.payload_json()?; println!("[online-delta] {:?} {}", d.kind, d.machine.machine_id); }
            FrameType::OnlineList => { let m: OnlineList = resp.payload_json()?; print_online(&m.machines); }
            _ => {}
        }
    };
    if r.source == "pool_unavailable" || r.memories.is_empty() {
        // 回退本地托底
        tracing::warn!("pool unavailable/empty → fallback to local ({})", fallback.base_url());
        let local = fallback.recall_local(query, "rec-local").await?;
        print_results("local", &local.memories);
    } else {
        print_results("pool", &r.memories);
    }
    Ok(())
}

fn print_results(source: &str, memories: &[nsmt_core::messages::MemoryHit]) {
    println!("=== recall source={source} hits={} ===", memories.len());
    for m in memories {
        println!("[{}] {}\n---\n{}", m.fqn, m.score, m.content);
    }
}

// ── M2：共享文件同步 ──

async fn fs_mode<R, W>(fs: &mut FrameStream<R, W>, fqn: &str) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let dir = fs::share_dir();
    std::fs::create_dir_all(&dir)?;
    tracing::info!("share dir: {}", dir.display());

    let mut local_tree = fs::build_tree(&dir)?;
    // 首次：本地对象入库 + 推对象 + 推树
    sync_push(fs, &local_tree, fqn).await?;
    tracing::info!("initial push done: {} entries, tree={}", local_tree.entries.len(), local_tree.tree_hash);

    // 文件监听
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    {
        let tx = tx.clone();
        let dir2 = dir.clone();
        tokio::spawn(async move {
            let mut watcher = notify::recommended_watcher(move |_res: notify::Result<notify::Event>| {
                let _ = tx.send(());
            })
            .expect("watcher");
            watcher.watch(&dir2, notify::RecursiveMode::Recursive).expect("watch");
            std::future::pending::<()>().await; // 保持 watcher 存活
        });
    }

    let mut last_pull = now_ms();
    loop {
        tokio::select! {
            _ = rx.recv() => {
                // 本地变更 → 重新建树、推送
                tokio::time::sleep(std::time::Duration::from_millis(200)).await; // 防抖
                if let Ok(t) = fs::build_tree(&dir) {
                    sync_push(fs, &t, fqn).await?;
                    local_tree = t;
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {
                if now_ms() - last_pull > 5_000 {
                    last_pull = now_ms();
                    // 远端 diff → 拉取
                    let diff = fs::request_diff(fs, &local_tree.tree_hash).await?;
                    if !diff.changed.is_empty() || !diff.removed.is_empty() {
                        tracing::info!("remote diff: changed={:?} removed={:?}", diff.changed, diff.removed);
                        if let Some(remote_tree) = &diff.tree {
                            for p in &diff.changed {
                                if let Some(e) = remote_tree.entries.iter().find(|e| &e.path == p) {
                                    if let Some(bytes) = fs::get_object(fs, &e.blob_id).await? {
                                        fs::materialize(e, &bytes)?;
                                        fs::ensure_object_local(&e.blob_id, &bytes)?;
                                    }
                                }
                            }
                        }
                        for p in &diff.removed {
                            let _ = std::fs::remove_file(dir.join(p));
                        }
                        if let Ok(t) = fs::build_tree(&dir) {
                            local_tree = t;
                        }
                    }
                }
            }
        }
    }
}

async fn sync_push<R, W>(fs: &mut FrameStream<R, W>, tree: &FileTree, fqn: &str) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    // 对象入库 + 推送（M2 简单：每个条目上锁+推对象）
    for e in &tree.entries {
        let obj = fs::objects_dir().join(&e.blob_id);
        if !obj.exists() {
            let bytes = std::fs::read(fs::share_dir().join(&e.path))?;
            fs::ensure_object_local(&e.blob_id, &bytes)?;
        }
        fs::push_entry(fs, e, fqn).await?;
    }
    fs::push_tree(fs, tree).await?;
    Ok(())
}

// ── 在线模式 ──

async fn online_mode<R, W>(fs: &mut FrameStream<R, W>, fqn: &str) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let dir = fs::share_dir();
    let _ = dir;
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
                        _ => {}
                    },
                    Ok(None) => break,
                    Err(e) => { tracing::error!("recv: {e}"); break; }
                }
            }
        }
    }
    let _ = fqn;
    Ok(())
}

fn print_online(machines: &[MachineInfo]) {
    println!("=== online machines ({}) ===", machines.len());
    for m in machines {
        println!("  {} agents={:?}", m.machine_id, m.agents);
    }
}
