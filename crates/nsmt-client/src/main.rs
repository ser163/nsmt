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
use nsmt_core::crypto::{load_or_create_domain_key, load_or_create_machine_key, KeyPair};
use nsmt_core::frame::FrameType;
use notify::Watcher as _;
use nsmt_core::identity::{generate_machine_id, AgentTag};
use nsmt_core::messages::{
    Auth, FileGet, FileTree, Heartbeat, Hello, HelloAck, MachineInfo, MemoryCapture, MemoryRecall,
    OnlineDelta, OnlineList, Register, RegisterAck,
};
use nsmt_core::frame::{Frame, FrameType as FT};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerifier};
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

    let domain_key = load_or_create_domain_key()?;
    let machine_key = load_or_create_machine_key()?;

    let fallback = memory::LocalFallback::from_env();
    let fqn = format!("{user_domain}/{machine_id}/{agent}");

    let command = args.get(2).map(|s| s.as_str());

    // 冲突合并 CLI（无需服务器连接）
    if command == Some("conflicts") || command == Some("merge") {
        return conflicts_cli(&args).await;
    }

    let conn = connect(server_addr).await?;
    tracing::info!("QUIC connected to {server_addr}");
    let (send, recv) = conn.open_bi().await?;
    let mut fs = FrameStream::new(recv, send);
    let peer_addr = start_peer_listener().await?;
    handshake(&mut fs, &user_domain, &agent, &machine_id, &peer_addr, &domain_key, &machine_key).await?;

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

async fn handshake<R, W>(
    fs: &mut FrameStream<R, W>,
    domain: &str,
    agent: &str,
    machine: &str,
    peer_addr: &str,
    domain_key: &KeyPair,
    machine_key: &KeyPair,
) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    fs.send_json(FrameType::Hello, 0, &Hello {
        user_domain: domain.into(),
        protocol_version: nsmt_core::frame::PROTOCOL_VERSION,
        client: format!("yggd/{}", env!("CARGO_PKG_VERSION")),
    }).await?;
    let ack: HelloAck = fs.recv().await?.ok_or_else(|| anyhow::anyhow!("eof"))?.payload_json()?;
    // AUTH：用域密钥签 nonce
    let nonce_sig = domain_key.sign(ack.nonce.as_bytes());
    fs.send_json(FrameType::Auth, 0, &Auth {
        user_domain: domain.into(),
        nonce_signature: nonce_sig,
    }).await?;
    // REGISTER：机器签名
    let msg = format!("{machine}\n{agent}");
    let machine_sig = machine_key.sign(msg.as_bytes());
    fs.send_json(FrameType::Register, 0, &Register {
        machine_id: machine.into(),
        agent_tag: agent.into(),
        peer_addr: peer_addr.into(),
        machine_pubkey: machine_key.public_hex(),
        machine_signature: machine_sig,
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
                                    tracing::info!("[pull] get_object {}", e.blob_id);
                                    let mut got = fs::get_object(fs, &e.blob_id).await?;
                                    tracing::info!("[pull] get_object done: {:?}", got.is_some());
                                    if got.is_none() {
                                        tracing::info!("[pull] server miss -> peer hint...");
                                        if let Some(peer) = peer_addr_hint(fs, &e.blob_id).await? {
                                            tracing::info!("[pull] peer hint = {peer}, connecting...");
                                            match fetch_from_peer(&peer, &e.blob_id).await {
                                                Ok(Some(b)) => { tracing::info!("[pull] peer fetch OK {} bytes", b.len()); got = Some(b); }
                                                Ok(None) => tracing::warn!("[pull] peer has no object"),
                                                Err(e) => tracing::warn!("[pull] peer fetch failed: {e}"),
                                            }
                                        } else {
                                            tracing::warn!("[pull] no peer hint");
                                        }
                                    }
                                    if let Some(bytes) = got {
                                        fs::materialize_with_conflict(e, &bytes, fqn)?;
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


/// 请求服务器告知某对象是否缺失 + 持有者 peer 地址（返回 peer 地址或 None）。
async fn peer_addr_hint<R, W>(fs: &mut FrameStream<R, W>, blob_id: &str) -> anyhow::Result<Option<String>>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    fs.send_json(FT::FileGet, 0, &FileGet { blob_id: blob_id.to_string(), chunk_index: Some(0) }).await?;
    loop {
        let f = fs.recv().await?.ok_or_else(|| anyhow::anyhow!("eof"))?;
        match f.frame_type {
            FT::FileChunk => return Ok(None), // 服务器有对象
            FT::Error => {
                let e: nsmt_core::messages::ErrorMsg = f.payload_json()?;
                if let Some(peer) = e.message.strip_prefix("object not found; peer=") {
                    return Ok(Some(peer.to_string()));
                }
                return Ok(None);
            }
            _ => {}
        }
    }
}

/// 从对端 peer 直连拉取对象（dev：不校验对端证书）。
async fn fetch_from_peer(peer_addr: &str, blob_id: &str) -> anyhow::Result<Option<Vec<u8>>> {
    let addr: SocketAddr = peer_addr.parse().context("bad peer addr")?;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let rustls_client = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(std::sync::Arc::new(NoVerify))
        .with_no_client_auth();
    let client_config = quinn::ClientConfig::new(std::sync::Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(rustls_client)?,
    ));
    let endpoint = quinn::Endpoint::client("0.0.0.0:0".parse()?)?;
    let conn = endpoint.connect_with(client_config, addr, "localhost").context("peer connect")?.await?;
    let (send, recv) = conn.open_bi().await?;
    let mut fs = FrameStream::new(recv, send);
    fs::get_object(&mut fs, blob_id).await
}

/// 启动 P2P 监听器（服务 FILE_GET），返回本机 peer 地址。
async fn start_peer_listener() -> anyhow::Result<String> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let key = rcgen::KeyPair::generate().context("rcgen key")?;
    let params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).context("params")?;
    let cert = params.self_signed(&key).context("self-sign")?;
    let cert_der = rustls::pki_types::CertificateDer::from(cert.der().clone());
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(
        rustls::pki_types::PrivatePkcs8KeyDer::from(key.serialize_der()),
    );
    let rustls_server = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)?;
    let server_config = quinn::ServerConfig::with_crypto(std::sync::Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(rustls_server)?,
    ));
    let bind = std::env::var("NSMT_PEER_PORT")
        .unwrap_or_else(|_| "127.0.0.1:0".into())
        .parse::<SocketAddr>()?;
    let endpoint = quinn::Endpoint::server(server_config, bind)?;
    let local = endpoint.local_addr()?;
    tokio::spawn(async move {
        while let Some(conn) = endpoint.accept().await {
            tokio::spawn(async move {
                if let Ok(c) = conn.await {
                    let _ = serve_peer(c).await;
                }
            });
        }
    });
    tracing::info!("P2P listener on {local}");
    Ok(local.to_string())
}

/// 对等节点服务：只处理 FILE_GET → FILE_CHUNK。
async fn serve_peer(conn: quinn::Connection) -> anyhow::Result<()> {
    let (send, recv) = conn.accept_bi().await?;
    let mut fs = FrameStream::new(recv, send);
    loop {
        match fs.recv().await? {
            Some(f) if f.frame_type == FT::FileGet => {
                let g: FileGet = f.payload_json()?;
                let obj = fs::objects_dir().join(&g.blob_id);
                if let Ok(data) = std::fs::read(&obj) {
                    let idx = g.chunk_index.unwrap_or(0);
                    let start = (idx * nsmt_core::frame::CHUNK_SIZE as u64) as usize;
                    let chunk = if start < data.len() {
                        data[start..std::cmp::min(start + nsmt_core::frame::CHUNK_SIZE, data.len())].to_vec()
                    } else {
                        Vec::new()
                    };
                    let mut payload = (idx as u32).to_le_bytes().to_vec();
                    payload.extend_from_slice(&chunk);
                    fs.send(&Frame::new(FT::FileChunk, 0, payload)).await?;
                } else {
                    let e = nsmt_core::messages::ErrorMsg { code: "0xE020".into(), message: "object not found".into(), request_id: None };
                    fs.send_json(FT::Error, 0, &e).await?;
                }
            }
            Some(_) => {}
            None => break,
        }
    }
    Ok(())
}


/// P2P dev 用：不校验对端证书。
#[derive(Debug)]
struct NoVerify;

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA256,
        ]
    }
}


// ── M4：冲突合并 CLI ──

/// `yggd conflicts [share_dir]`：列出冲突副本。
/// `yggd merge <冲突文件> [--keep-local|--keep-remote]`：解决冲突。
async fn conflicts_cli(args: &[String]) -> anyhow::Result<()> {
    let cmd = args.get(2).map(|s| s.as_str()).unwrap_or("");
    let dir = fs::share_dir();
    if cmd == "conflicts" {
        let mut found = false;
        for e in std::fs::read_dir(&dir)? {
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with(".sync-conflict-") {
                found = true;
                let meta = e.metadata()?;
                println!("{}  ({} bytes)", name, meta.len());
            }
        }
        if !found {
            println!("（无冲突副本）");
        }
        return Ok(());
    }
    if cmd == "merge" {
        let conflict = args.get(3).context("usage: yggd merge <冲突文件> [--keep-local|--keep-remote]")?;
        let conflict_path = if conflict.starts_with('/') || conflict.starts_with('.') {
            std::path::PathBuf::from(conflict)
        } else {
            dir.join(conflict)
        };
        if !conflict_path.exists() {
            anyhow::bail!("冲突文件不存在: {}", conflict_path.display());
        }
        // 主文件名 = 去掉前缀 .sync-conflict-<machine>- 和时间戳后缀
        let name = conflict_path.file_name().unwrap().to_string_lossy().into_owned();
        // 形式：.sync-conflict-<machine>-<path>-<ts>；主文件由 <path> 决定（去掉尾部 -<ts>）
        let stripped = name.strip_prefix(".sync-conflict-").unwrap_or(&name);
        let ts_idx = stripped.rfind('-').map(|i| i).unwrap_or(0);
        let machine_and_path = &stripped[..ts_idx];
        let dash = machine_and_path.find('-');
        let main_name = match dash {
            Some(i) => &machine_and_path[i + 1..],
            None => machine_and_path,
        };
        let main_path = dir.join(main_name);
        let conflict_bytes = std::fs::read(&conflict_path)?;

        let keep = args.iter().find_map(|a| match a.as_str() {
            "--keep-local" => Some("local"),
            "--keep-remote" => Some("remote"),
            _ => None,
        });
        match keep {
            Some("local") => {
                // 保留冲突副本（本地修改），覆盖主文件
                std::fs::write(&main_path, &conflict_bytes)?;
                std::fs::remove_file(&conflict_path)?;
                println!("已保留本地版本并覆盖 {}，删除冲突副本", main_path.display());
            }
            Some("remote") => {
                // 保留主文件（远端版本），删除冲突副本
                std::fs::remove_file(&conflict_path)?;
                println!("已保留远端版本（{}），删除冲突副本", main_path.display());
            }
            _ => {
                println!("冲突文件: {}", conflict_path.display());
                println!("主文件: {}", main_path.display());
                println!("冲突内容（本地修改版）:\n{}", String::from_utf8_lossy(&conflict_bytes));
                let main_bytes = std::fs::read(&main_path).unwrap_or_default();
                println!("主文件内容（远端版）:\n{}", String::from_utf8_lossy(&main_bytes));
                use std::io::Write as _;
                print!("解决方式 [l]ocal=保留本地修改 / [r]emote=保留远端 / [c]ancel: ");
                std::io::stdout().flush()?;
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                match input.trim() {
                    "l" | "L" => {
                        std::fs::write(&main_path, &conflict_bytes)?;
                        std::fs::remove_file(&conflict_path)?;
                        println!("已保留本地修改并覆盖主文件");
                    }
                    "r" | "R" => {
                        std::fs::remove_file(&conflict_path)?;
                        println!("已保留远端版本，删除冲突副本");
                    }
                    _ => println!("已取消"),
                }
            }
        }
        return Ok(());
    }
    Ok(())
}
