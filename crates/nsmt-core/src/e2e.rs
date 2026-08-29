//! 端到端加密（protocol.md §10）：文件块/载荷可选加密（flags.bit0）。
//!
//! 用 ChaCha20-Poly1305；密钥来自环境变量 `NSMT_E2E_KEY`（32 字节 hex）。
//! 未设置时返回 `None`（不加密）。nonce 由 chunk_index + 固定 salt 派生。

use crate::error::NsmtError;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};

/// 从环境读取 E2E 密钥；未设置返回 None。
pub fn e2e_key_from_env() -> Option<ChaCha20Poly1305> {
    let hexkey = std::env::var("NSMT_E2E_KEY").ok()?;
    let bytes = hex::decode(hexkey).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    Some(ChaCha20Poly1305::new(Key::from_slice(&bytes)))
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
}
