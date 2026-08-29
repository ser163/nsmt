//! 租户注册表与鉴权（protocol.md §2.3）。
//!
//! - 租户记录：`user_domain → (domain_pubkey, machines: {machine_id → machine_pubkey})`
//! - 引导：从 `NSMT_HOME/tenants.json` 加载；`ygg admin add-tenant` 可追加
//! - 验签：AUTH 用 domain key 验 nonce 签名；REGISTER 用 machine key 验机器签名

use nsmt_core::crypto::KeyPair;
use nsmt_core::error::NsmtError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TenantRecord {
    pub domain_pubkey: String,
    #[serde(default)]
    pub machines: HashMap<String, String>, // machine_id -> machine_pubkey
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct TenantFile {
    tenants: HashMap<String, TenantRecord>,
}

#[derive(Default, Clone)]
pub struct TenantStore {
    inner: Arc<RwLock<HashMap<String, TenantRecord>>>,
}

impl TenantStore {
    pub fn tenants_path() -> PathBuf {
        std::env::var("NSMT_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::var("HOME")
                    .map(|h| PathBuf::from(h).join(".nsmt"))
                    .unwrap_or_else(|_| PathBuf::from(".nsmt"))
            })
            .join("tenants.json")
    }

    /// 从磁盘加载（文件不存在则空）。
    pub async fn load() -> Self {
        let store = Self::default();
        let _ = store.reload().await;
        store
    }

    pub async fn reload(&self) -> Result<(), NsmtError> {
        let p = Self::tenants_path();
        if !p.exists() {
            return Ok(());
        }
        let raw = std::fs::read_to_string(&p).map_err(NsmtError::Io)?;
        let file: TenantFile = serde_json::from_str(&raw).map_err(NsmtError::Json)?;
        let mut g = self.inner.write().await;
        *g = file.tenants;
        Ok(())
    }

    /// 持久化当前租户表。
    pub async fn persist(&self) -> Result<(), NsmtError> {
        let g = self.inner.read().await;
        let file = TenantFile { tenants: g.clone() };
        drop(g);
        std::fs::create_dir_all(Self::tenants_path().parent().unwrap_or(std::path::Path::new(".")))
            .map_err(NsmtError::Io)?;
        std::fs::write(Self::tenants_path(), serde_json::to_vec_pretty(&file).map_err(NsmtError::Json)?)
            .map_err(NsmtError::Io)
    }

    /// 添加/更新租户（域公钥）。
    pub async fn upsert_tenant(&self, domain: &str, domain_pubkey: &str) -> Result<(), NsmtError> {
        let mut g = self.inner.write().await;
        g.entry(domain.to_string())
            .or_default()
            .domain_pubkey = domain_pubkey.to_string();
        drop(g);
        self.persist().await
    }

    /// AUTH 验签：用租户域公钥验证 nonce 签名。
    pub async fn verify_auth(&self, domain: &str, nonce: &str, signature: &str) -> Result<(), String> {
        let g = self.inner.read().await;
        match g.get(domain) {
            Some(t) => {
                if KeyPair::verify(&t.domain_pubkey, nonce.as_bytes(), signature) {
                    Ok(())
                } else {
                    Err("auth_failed".into())
                }
            }
            None => Err("tenant_not_found".into()),
        }
    }

    /// REGISTER：校验机器签名；机器已注册时校验公钥一致（防劫持）。
    pub async fn verify_register(
        &self,
        domain: &str,
        machine_id: &str,
        agent_tag: &str,
        machine_pubkey: &str,
        signature: &str,
    ) -> Result<(), String> {
        let msg = format!("{machine_id}\n{agent_tag}");
        if !KeyPair::verify(machine_pubkey, msg.as_bytes(), signature) {
            return Err("bad_machine_signature".into());
        }
        let mut g = self.inner.write().await;
        let t = g.get_mut(domain).ok_or("tenant_not_found")?;
        if let Some(existing) = t.machines.get(machine_id) {
            if existing != machine_pubkey {
                return Err("machine_hijack_detected".into());
            }
        } else {
            t.machines.insert(machine_id.to_string(), machine_pubkey.to_string());
        }
        // 机器表只在内存维护（避免多进程陈旧表覆盖磁盘）；域公钥由 admin 持久化
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nsmt_core::crypto::KeyPair;

    #[tokio::test]
    async fn auth_verify_flow() {
        let domain_key = KeyPair::generate();
        let store = TenantStore::default();
        store.upsert_tenant("ser163", &domain_key.public_hex()).await.unwrap();

        let nonce = "nsmt-nonce-abc";
        let sig = domain_key.sign(nonce.as_bytes());
        assert!(store.verify_auth("ser163", nonce, &sig).await.is_ok());
        assert!(store.verify_auth("ser163", nonce, "bad").await.is_err());
        assert!(store.verify_auth("nobody", nonce, &sig).await.is_err());
    }

    #[tokio::test]
    async fn register_verify_and_hijack() {
        let domain_key = KeyPair::generate();
        let machine_key = KeyPair::generate();
        let other_machine = KeyPair::generate();
        let store = TenantStore::default();
        store.upsert_tenant("ser163", &domain_key.public_hex()).await.unwrap();

        let msg = format!("m1\nmaka");
        let sig = machine_key.sign(msg.as_bytes());
        assert!(store
            .verify_register("ser163", "m1", "maka", &machine_key.public_hex(), &sig)
            .await
            .is_ok());
        // 同机器重注册（同公钥，新 agent 用新消息签名）→ OK
        let msg2 = format!("m1
hermes");
        let sig2 = machine_key.sign(msg2.as_bytes());
        assert!(store
            .verify_register("ser163", "m1", "hermes", &machine_key.public_hex(), &sig2)
            .await
            .is_ok());
        // 不同公钥同 machine_id → 劫持拒绝
        let sig2 = other_machine.sign(msg.as_bytes());
        assert!(store
            .verify_register("ser163", "m1", "maka", &other_machine.public_hex(), &sig2)
            .await
            .is_err());
    }
}
