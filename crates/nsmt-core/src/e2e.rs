//! 端到端加密（protocol.md §10）：文件块/载荷可选加密（flags.bit0）。
//!
//! M9.4：支持**密钥轮换**与**按租户密钥**：
//! - `NSMT_E2E_KEYS`（逗号分隔，**最新在前**）或 `NSMT_E2E_KEY`（单密钥，兼容）配置主密钥；
//!   未配置 → 不加密（返回 None）。
//! - 加密总是用最新密钥；解密依次尝试全部密钥（旧密钥保留即可解密历史数据 → 轮换无缝）。
//! - 按租户：`derive_tenant_key(master, domain)` 从主密钥派生每租户密钥
//!   （`SHA-256("nsmt:e2e:v1:" || domain || master)`），server/client 同域派生一致，
//!   无需在网络上分发密钥。
//!
//! 轮换流程：把新密钥写到 `NSMT_E2E_KEYS` 最前面，重启两端；旧密钥留在列表里直到
//! 所有节点升级完成后再移除。

use crate::error::NsmtError;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use sha2::{Digest, Sha256};

/// E2E 密钥集：最新密钥用于加密；全部密钥用于解密（轮换）。
pub struct E2EKeys {
    newest: ChaCha20Poly1305,
    all: Vec<ChaCha20Poly1305>,
}

fn cipher_from_bytes(bytes: &[u8]) -> Option<ChaCha20Poly1305> {
    if bytes.len() != 32 {
        return None;
    }
    Some(ChaCha20Poly1305::new(Key::from_slice(bytes)))
}

/// 从主密钥派生租户密钥：`SHA-256("nsmt:e2e:v1:" || domain || master)`。
pub fn derive_tenant_key(master: &[u8], domain: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"nsmt:e2e:v1:");
    h.update(domain.as_bytes());
    h.update(master);
    let d = h.finalize();
    let mut k = [0u8; 32];
    k.copy_from_slice(&d);
    k
}

impl E2EKeys {
    /// 从环境读取主密钥列表（最新在前）；未配置返回 None。
    pub fn masters_from_env() -> Option<Vec<[u8; 32]>> {
        let raw = std::env::var("NSMT_E2E_KEYS")
            .or_else(|_| std::env::var("NSMT_E2E_KEY"))
            .ok()?;
        let list: Vec<[u8; 32]> = raw
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .filter_map(|h| hex::decode(h).ok())
            .filter(|b| b.len() == 32)
            .map(|b| {
                let mut k = [0u8; 32];
                k.copy_from_slice(&b);
                k
            })
            .collect();
        if list.is_empty() {
            None
        } else {
            Some(list)
        }
    }

    /// 从环境构造密钥集。`domain` 非空 → 每把主密钥派生为租户密钥（per-tenant）。
    pub fn from_env(domain: Option<&str>) -> Option<Self> {
        let masters = Self::masters_from_env()?;
        let all: Vec<ChaCha20Poly1305> = masters
            .iter()
            .map(|m| {
                let bytes = match domain {
                    Some(d) if !d.is_empty() => derive_tenant_key(m, d).to_vec(),
                    _ => m.to_vec(),
                };
                cipher_from_bytes(&bytes)
            })
            .collect::<Option<_>>()?;
        let newest = all[0].clone();
        Some(Self { newest, all })
    }

    pub fn is_configured(&self) -> bool {
        true
    }

    /// 用最新密钥加密。
    pub fn encrypt(&self, payload: &[u8], idx: u64) -> Result<Vec<u8>, NsmtError> {
        self.newest
            .encrypt(&nonce_for(idx), Payload { msg: payload, aad: &[] })
            .map_err(|_| NsmtError::Protocol("encrypt failed".into()))
    }

    /// 依次尝试全部密钥解密（轮换：旧密钥继续可用）。
    pub fn decrypt(&self, payload: &[u8], idx: u64) -> Result<Vec<u8>, NsmtError> {
        for c in &self.all {
            if let Ok(plain) = c.decrypt(&nonce_for(idx), Payload { msg: payload, aad: &[] }) {
                return Ok(plain);
            }
        }
        Err(NsmtError::Protocol("decrypt failed (no key matched)".into()))
    }
}

/// 兼容入口：读取单密钥（无域派生）。返回 None 表示未配置。
pub fn e2e_key_from_env() -> Option<ChaCha20Poly1305> {
    let hexkey = std::env::var("NSMT_E2E_KEY").ok()?;
    cipher_from_bytes(&hex::decode(hexkey).ok()?)
}

fn nonce_for(idx: u64) -> Nonce {
    let mut n = [0u8; 12];
    n[0..4].copy_from_slice(&0x4E_53_4D_54u32.to_be_bytes()); // "NSMT"
    n[4..12].copy_from_slice(&idx.to_be_bytes());
    Nonce::from(n)
}

/// 加密载荷；未配密钥时原样返回（flags 由调用方决定是否置 bit0）。
pub fn encrypt_payload(cipher: Option<&ChaCha20Poly1305>, payload: &[u8], idx: u64) -> Result<Vec<u8>, NsmtError> {
    match cipher {
        None => Ok(payload.to_vec()),
        Some(c) => c
            .encrypt(&nonce_for(idx), Payload { msg: payload, aad: &[] })
            .map_err(|_| NsmtError::Protocol("encrypt failed".into())),
    }
}

/// 解密载荷；未配密钥时原样返回。
pub fn decrypt_payload(cipher: Option<&ChaCha20Poly1305>, payload: &[u8], idx: u64) -> Result<Vec<u8>, NsmtError> {
    match cipher {
        None => Ok(payload.to_vec()),
        Some(c) => c
            .decrypt(&nonce_for(idx), Payload { msg: payload, aad: &[] })
            .map_err(|_| NsmtError::Protocol("decrypt failed".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let c = ChaCha20Poly1305::new(Key::from_slice(&[7u8; 32]));
        let data = b"hello encrypted world";
        let enc = encrypt_payload(Some(&c), data, 42).unwrap();
        assert_ne!(enc, data);
        let dec = decrypt_payload(Some(&c), &enc, 42).unwrap();
        assert_eq!(dec, data);
    }

    #[test]
    fn no_key_passthrough() {
        let data = b"plain";
        assert_eq!(encrypt_payload(None, data, 0).unwrap(), data);
        assert_eq!(decrypt_payload(None, data, 0).unwrap(), data);
    }

    #[test]
    fn tenant_keys_derive_stably_and_differ() {
        let master = [9u8; 32];
        let a = derive_tenant_key(&master, "ser163");
        let b = derive_tenant_key(&master, "ser163");
        let c = derive_tenant_key(&master, "other");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn rotation_decrypts_with_old_key() {
        let old = E2EKeys { newest: cipher_from_bytes(&[1u8; 32]).unwrap(), all: vec![cipher_from_bytes(&[1u8; 32]).unwrap()] };
        let data = b"old data";
        let enc = old.encrypt(data, 0).unwrap();
        // 轮换后：新密钥加密 + 新旧都保留解密
        let rotated = E2EKeys {
            newest: cipher_from_bytes(&[2u8; 32]).unwrap(),
            all: vec![
                cipher_from_bytes(&[2u8; 32]).unwrap(),
                cipher_from_bytes(&[1u8; 32]).unwrap(),
            ],
        };
        assert_eq!(rotated.decrypt(&enc, 0).unwrap(), data);
        let new_enc = rotated.encrypt(data, 0).unwrap();
        assert_eq!(rotated.decrypt(&new_enc, 0).unwrap(), data);
    }

    #[test]
    fn keys_from_env_parses_list() {
        // 64 hex chars = 32 字节
        let k1 = "ab".repeat(32);
        let k2 = "cd".repeat(32);
        std::env::set_var("NSMT_E2E_KEYS", format!("{k1},{k2}"));
        let keys = E2EKeys::from_env(Some("ser163")).unwrap();
        assert!(keys.is_configured());
        let data = b"x";
        let enc = keys.encrypt(data, 1).unwrap();
        assert_eq!(keys.decrypt(&enc, 1).unwrap(), data);
        std::env::remove_var("NSMT_E2E_KEYS");
    }
}
