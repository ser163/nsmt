//! NSMT 服务器 `ygg` — M0：QUIC 监听 + 握手/注册 + 在线注册表 + 广播。

mod fs;
mod memory;
mod registry;
mod session;
mod state;

use anyhow::Context;
use std::net::SocketAddr;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    // rustls 双 provider 存在时需显式选择（quinn 拉 aws-lc-rs + ring）
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let bind: SocketAddr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "0.0.0.0:5555".into())
        .parse()
        .context("usage: ygg [bind_addr]")?;

    let (cert_der, key_der) = tls::self_signed("localhost")?;
    persist_cert(&cert_der)?;
    let rustls_server = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .context("build rustls server config")?;
    let server_config = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(rustls_server)
            .context("quic server config")?,
    ));
    let endpoint =
        quinn::Endpoint::server(server_config, bind).context("bind quinn endpoint")?;

    let state = Arc::new(state::ServerState::new());
    tokio::spawn(state.registry.clone().prune_loop());
    tokio::spawn(state.locks.clone().cleanup_loop());

    tracing::info!("ygg listening on {bind} (QUIC)");

    while let Some(conn) = endpoint.accept().await {
        let state = state.clone();
        tokio::spawn(async move {
            match conn.await {
                Ok(c) => {
                    if let Err(e) = session::handle(c, state).await {
                        tracing::debug!("session ended: {e}");
                    }
                }
                Err(e) => tracing::debug!("accept failed: {e}"),
            }
        });
    }

    Ok(())
}

/// 把服务器证书写盘，供客户端作根证书信任（M0 开发用）。
fn persist_cert(cert: &rustls::pki_types::CertificateDer<'_>) -> anyhow::Result<()> {
    let dir = std::env::var("NSMT_HOME").unwrap_or_else(|_| {
        std::env::var("HOME").map(|h| format!("{h}/.nsmt")).unwrap_or_else(|_| ".nsmt".into())
    });
    std::fs::create_dir_all(&dir)?;
    std::fs::write(format!("{dir}/ygg.crt"), cert.as_ref())?;
    tracing::info!("server cert written to {dir}/ygg.crt");
    Ok(())
}

/// M0 用自签名证书（正式版换 CA/Let's Encrypt）。
mod tls {
    use anyhow::Context;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

    pub fn self_signed(host: &str) -> anyhow::Result<(CertificateDer<'static>, PrivateKeyDer<'static>)> {
        let key = rcgen::KeyPair::generate()
            .context("generate rcgen key")?;
        let params = rcgen::CertificateParams::new(vec![host.to_string()])
            .context("cert params")?;
        let cert = params.self_signed(&key).context("self-sign")?;
        let cert_der = CertificateDer::from(cert.der().clone());
        let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        Ok((cert_der, key_der))
    }
}
