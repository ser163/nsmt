//! 身份加密（protocol.md §2.2、§2.3）：Ed25519 密钥生成、签名、持久化。
//!
//! - 机器级 `identity.key`：机器身份签名、TLS 客户端证书绑定
//! - 用户域级 `domain.key`：签发机器凭证、AUTH nonce 签名

use crate::error::NsmtError;
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use std::path::{Path, PathBuf};

/// 密钥对（Ed25519）。
#[derive(Debug, Clone)]
pub struct KeyPair {
    signing: SigningKey,
    verifying: VerifyingKey,
}

impl KeyPair {
    pub fn generate() -> Self {
        let signing = SigningKey::generate(&mut rand_core::OsRng);
        Self::from_signing(signing)
    }

    fn from_signing(signing: SigningKey) -> Self {
        let verifying = signing.verifying_key();
        Self { signing, verifying }
    }

    pub fn public_hex(&self) -> String {
        hex::encode(self.verifying.as_bytes())
    }

    pub fn sign(&self, msg: &[u8]) -> String {
        hex::encode(self.signing.sign(msg).to_bytes())
    }

    /// 验证签名（hex）。`msg` 与签名须一一对应。
    pub fn verify(public_hex: &str, msg: &[u8], sig_hex: &str) -> bool {
        let Ok(pk) = hex::decode(public_hex) else { return false };
        let Ok(pk) = VerifyingKey::from_bytes(pk.as_slice().try_into().unwrap_or(&[0u8; 32])) else {
            return false;
        };
        let Ok(sig) = hex::decode(sig_hex) else { return false };
        let Ok(sig) = ed25519_dalek::Signature::from_slice(&sig) else {
            return false;
        };
        pk.verify(msg, &sig).is_ok()
    }

    /// 序列化为 PKCS8 DER（持久化）。
    pub fn to_pkcs8_der(&self) -> Vec<u8> {
        self.signing.to_bytes().to_vec()
    }

    /// 从 PKCS8 DER 还原。
    pub fn from_pkcs8_der(der: &[u8]) -> Result<Self, NsmtError> {
        let bytes: [u8; 32] = der
            .try_into()
            .map_err(|_| NsmtError::Protocol("invalid key length".into()))?;
        Ok(Self::from_signing(SigningKey::from_bytes(&bytes)))
    }
}

/// 默认密钥目录。
pub fn keys_dir() -> PathBuf {
    std::env::var("NSMT_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(|h| PathBuf::from(h).join(".nsmt"))
                .unwrap_or_else(|_| PathBuf::from(".nsmt"))
        })
}

/// 加载或生成机器级密钥。
pub fn load_or_create_machine_key() -> Result<KeyPair, NsmtError> {
    load_or_create("machine.key", "machine.pub")
}

/// 加载或生成域级密钥。
pub fn load_or_create_domain_key() -> Result<KeyPair, NsmtError> {
    load_or_create("domain.key", "domain.pub")
}

fn load_or_create(key_name: &str, pub_name: &str) -> Result<KeyPair, NsmtError> {
    let dir = keys_dir();
    std::fs::create_dir_all(&dir).map_err(NsmtError::Io)?;
    let key_path: PathBuf = dir.join(key_name);
    let pub_path: PathBuf = dir.join(pub_name);

    if key_path.exists() {
        let der = std::fs::read(&key_path).map_err(NsmtError::Io)?;
        let kp = KeyPair::from_pkcs8_der(&der)?;
        // 校验 pub 文件是否一致（不一致则报错，防密钥被改）
        if let Ok(pub_hex) = std::fs::read_to_string(&pub_path) {
            if pub_hex.trim() != kp.public_hex() {
                return Err(NsmtError::Protocol(format!(
                    "key mismatch: {key_name} vs {pub_name}"
                )));
            }
        }
        return Ok(kp);
    }

    let kp = KeyPair::generate();
    std::fs::write(&key_path, kp.to_pkcs8_der()).map_err(NsmtError::Io)?;
    std::fs::write(&pub_path, kp.public_hex()).map_err(NsmtError::Io)?;
    Ok(kp)
}

/// 读取某个公钥文件（hex）。
pub fn read_public_key(path: &Path) -> Result<String, NsmtError> {
    Ok(std::fs::read_to_string(path)
        .map_err(NsmtError::Io)?
        .trim()
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_verify_roundtrip() {
        let kp = KeyPair::generate();
        let msg = b"nsmt-nonce-123";
        let sig = kp.sign(msg);
        assert!(KeyPair::verify(&kp.public_hex(), msg, &sig));
        assert!(!KeyPair::verify(&kp.public_hex(), b"tampered", &sig));
    }

    #[test]
    fn persist_roundtrip() {
        let kp = KeyPair::generate();
        let der = kp.to_pkcs8_der();
        let restored = KeyPair::from_pkcs8_der(&der).unwrap();
        assert_eq!(kp.public_hex(), restored.public_hex());
        let sig = kp.sign(b"x");
        assert!(KeyPair::verify(&restored.public_hex(), b"x", &sig));
    }
}
