//! 协议错误码（protocol.md §9）。

use serde::{Deserialize, Serialize};

/// 协议错误码（protocol.md §9）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u16)]
pub enum ErrorCode {
    // 身份/鉴权
    TenantNotFound = 0xE001,
    AuthFailed = 0xE002,
    TicketExpired = 0xE003,
    NotRegistered = 0xE004,
    TenantForbidden = 0xE005,
    UnsupportedVersion = 0xE006,
    // 记忆
    MemoryPoolUnavailable = 0xE010,
    MemoryRecallTimeout = 0xE011,
    MemoryCaptureConflict = 0xE012,
    // 文件
    ObjectNotFound = 0xE020,
    TreeConflict = 0xE021,
    // 锁
    LockHeld = 0xE030,
    LockTimeout = 0xE031,
    // 资源
    QuotaExceeded = 0xE040,
    RateLimited = 0xE041,
    // 其它
    InternalError = 0xE0FF,
}

impl ErrorCode {
    /// 从原始 u16 解码，未知码返回 [`ErrorCode::InternalError`]。
    pub fn from_raw(raw: u16) -> Self {
        match raw {
            0xE001 => Self::TenantNotFound,
            0xE002 => Self::AuthFailed,
            0xE003 => Self::TicketExpired,
            0xE004 => Self::NotRegistered,
            0xE005 => Self::TenantForbidden,
            0xE006 => Self::UnsupportedVersion,
            0xE010 => Self::MemoryPoolUnavailable,
            0xE011 => Self::MemoryRecallTimeout,
            0xE012 => Self::MemoryCaptureConflict,
            0xE020 => Self::ObjectNotFound,
            0xE021 => Self::TreeConflict,
            0xE030 => Self::LockHeld,
            0xE031 => Self::LockTimeout,
            0xE040 => Self::QuotaExceeded,
            0xE041 => Self::RateLimited,
            _ => Self::InternalError,
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "0x{:04X}", *self as u16)
    }
}

/// 协议层错误。
#[derive(Debug, thiserror::Error)]
pub enum NsmtError {
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("frame too large: {0}")]
    FrameTooLarge(usize),
    #[error("invalid magic: {0:#x}")]
    InvalidMagic(u8),
    #[error("unsupported version: {0}")]
    UnsupportedVersion(u8),
    #[error("invalid fqn: {0}")]
    InvalidFqn(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}
