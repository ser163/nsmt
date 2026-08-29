//! 身份与命名空间（protocol.md §2）。
//!
//! FQN = `<user_domain>/<machine_id>/<agent_tag>`。

use crate::error::NsmtError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 用户域（租户键）：`[a-z0-9][a-z0-9._-]{2,63}`。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UserDomain(String);

/// 机器码：16 位小写 hex。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MachineId(String);

/// Agent 标识：`[a-zA-Z0-9._-]{1,63}`。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentTag(String);

/// 全局限定名。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Fqn {
    pub user_domain: UserDomain,
    pub machine_id: MachineId,
    pub agent_tag: AgentTag,
}

impl UserDomain {
    pub fn new(s: impl Into<String>) -> Result<Self, NsmtError> {
        let s = s.into();
        let valid = !s.is_empty()
            && s.len() <= 64
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c));
        if !valid {
            return Err(NsmtError::InvalidFqn(format!(
                "invalid user_domain: {s:?}"
            )));
        }
        Ok(Self(s))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AgentTag {
    pub fn new(s: impl Into<String>) -> Result<Self, NsmtError> {
        let s = s.into();
        let valid = !s.is_empty()
            && s.len() <= 63
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
        if !valid {
            return Err(NsmtError::InvalidFqn(format!("invalid agent_tag: {s:?}")));
        }
        Ok(Self(s))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl MachineId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for MachineId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::fmt::Display for Fqn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}/{}/{}",
            self.user_domain.as_str(),
            self.machine_id,
            self.agent_tag.as_str()
        )
    }
}

impl std::str::FromStr for Fqn {
    type Err = NsmtError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut parts = s.splitn(3, '/');
        let user_domain = parts.next().ok_or(NsmtError::InvalidFqn(s.into()))?;
        let machine_id = parts.next().ok_or(NsmtError::InvalidFqn(s.into()))?;
        let agent_tag = parts.next().ok_or(NsmtError::InvalidFqn(s.into()))?;
        if machine_id.len() != 16 || !machine_id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(NsmtError::InvalidFqn(format!(
                "invalid machine_id: {machine_id:?}"
            )));
        }
        Ok(Self {
            user_domain: UserDomain::new(user_domain)?,
            machine_id: MachineId(machine_id.to_lowercase()),
            agent_tag: AgentTag::new(agent_tag)?,
        })
    }
}

/// 生成稳定机器码（protocol.md §2.1）。
///
/// 输入：硬件 UUID（machine-uid）+ hostname + OS 信息 → SHA-256 前 16 hex。
/// 某些环境下取不到硬件 UUID 时，退化为 hostname+OS 哈希并标记 `stable=false`。
pub fn generate_machine_id() -> (MachineId, bool) {
    let hw = machine_uid::get().ok();
    let host = hostname::get().ok().map(|h| h.to_string_lossy().into_owned());
    let os = format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH);

    let mut h = Sha256::new();
    if let Some(hw) = &hw {
        h.update(hw.as_bytes());
    }
    if let Some(host) = &host {
        h.update(host.as_bytes());
    }
    h.update(os.as_bytes());
    let digest = h.finalize();
    let id = hex::encode(&digest[..8]); // 16 hex
    (MachineId(id), hw.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn fqn_parse_ok() {
        let fqn = Fqn::from_str("ser163/9f2c81d4deadbeef/maka").unwrap();
        assert_eq!(fqn.to_string(), "ser163/9f2c81d4deadbeef/maka");
    }

    #[test]
    fn fqn_parse_bad_machine() {
        assert!(Fqn::from_str("ser163/xyz/maka").is_err());
        assert!(Fqn::from_str("ser163/9f2c81d4deadbeef").is_err());
    }

    #[test]
    fn machine_id_is_16_hex() {
        let (id, _) = generate_machine_id();
        assert_eq!(id.as_str().len(), 16);
        assert!(id.as_str().bytes().all(|b| b.is_ascii_hexdigit()));
    }
}
