//! 二进制帧编解码（protocol.md §3.3、§4）。

use crate::error::NsmtError;

/// 帧头魔数 'Y'（Yggdrasil）。
pub const MAGIC: u8 = 0x59;
/// 协议版本（v0.1）。
pub const PROTOCOL_VERSION: u8 = 1;
/// 文件分块大小（1 MiB）。
pub const CHUNK_SIZE: usize = 1024 * 1024;
/// 单帧 payload 上限（16 MiB）。
pub const MAX_PAYLOAD_LEN: u32 = 16 * 1024 * 1024;

/// 帧类型（protocol.md §4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameType {
    Hello = 0x01,
    HelloAck = 0x02,
    Auth = 0x03,
    Ticket = 0x04,
    Register = 0x05,
    RegisterAck = 0x06,
    Heartbeat = 0x10,
    OnlineList = 0x11,
    OnlineDelta = 0x12,
    MemoryRecall = 0x20,
    MemoryRecallResult = 0x21,
    MemoryCapture = 0x22,
    MemoryCaptureResult = 0x23,
    FileTree = 0x30,
    FileDiff = 0x31,
    FileDiffResult = 0x32,
    FileGet = 0x33,
    FilePut = 0x34,
    FileChunk = 0x35,
    FilePutAck = 0x36,
    LockAcquire = 0x40,
    LockRenew = 0x41,
    LockRelease = 0x42,
    LockGranted = 0x43,
    LockDenied = 0x44,
    LockNotify = 0x45,
    PeerHello = 0x50,
    PeerAuth = 0x51,
    PeerAuthOk = 0x52,
    PeerHint = 0x53,
    Error = 0xF0,
}

impl FrameType {
    pub fn from_raw(raw: u8) -> Option<Self> {
        Some(match raw {
            0x01 => Self::Hello,
            0x02 => Self::HelloAck,
            0x03 => Self::Auth,
            0x04 => Self::Ticket,
            0x05 => Self::Register,
            0x06 => Self::RegisterAck,
            0x10 => Self::Heartbeat,
            0x11 => Self::OnlineList,
            0x12 => Self::OnlineDelta,
            0x20 => Self::MemoryRecall,
            0x21 => Self::MemoryRecallResult,
            0x22 => Self::MemoryCapture,
            0x23 => Self::MemoryCaptureResult,
            0x30 => Self::FileTree,
            0x31 => Self::FileDiff,
            0x32 => Self::FileDiffResult,
            0x33 => Self::FileGet,
            0x34 => Self::FilePut,
            0x35 => Self::FileChunk,
            0x36 => Self::FilePutAck,
            0x40 => Self::LockAcquire,
            0x41 => Self::LockRenew,
            0x42 => Self::LockRelease,
            0x43 => Self::LockGranted,
            0x44 => Self::LockDenied,
            0x45 => Self::LockNotify,
            0x50 => Self::PeerHello,
            0x51 => Self::PeerAuth,
            0x52 => Self::PeerAuthOk,
            0x53 => Self::PeerHint,
            0xF0 => Self::Error,
            _ => return None,
        })
    }
}

/// 帧标志位（protocol.md §3.3）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Flags(pub u8);

impl Flags {
    /// bit0：端到端加密。
    pub fn e2e_encrypted(&self) -> bool {
        self.0 & 0x01 != 0
    }
    /// bit1：payload 压缩。
    pub fn compressed(&self) -> bool {
        self.0 & 0x02 != 0
    }
}

/// 一帧数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub frame_type: FrameType,
    pub flags: Flags,
    pub stream_id: u32,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn new(frame_type: FrameType, stream_id: u32, payload: Vec<u8>) -> Self {
        Self {
            frame_type,
            flags: Flags::default(),
            stream_id,
            payload,
        }
    }

    /// 把 payload 当 JSON 反序列化。
    pub fn payload_json<T: serde::de::DeserializeOwned>(&self) -> Result<T, NsmtError> {
        Ok(serde_json::from_slice(&self.payload)?)
    }

    /// 从可序列化对象构造一帧（payload=JSON）。
    pub fn from_json<T: serde::Serialize>(
        frame_type: FrameType,
        stream_id: u32,
        value: &T,
    ) -> Result<Self, NsmtError> {
        Ok(Self::new(
            frame_type,
            stream_id,
            serde_json::to_vec(value)?,
        ))
    }
}

/// 帧编解码器（wire 格式，见 protocol.md §3.3）。
#[derive(Debug, Default)]
pub struct FrameCodec;

impl FrameCodec {
    /// 编码一帧为 wire 字节。
    pub fn encode(frame: &Frame) -> Result<Vec<u8>, NsmtError> {
        if frame.payload.len() > MAX_PAYLOAD_LEN as usize {
            return Err(NsmtError::FrameTooLarge(frame.payload.len()));
        }
        let mut buf = Vec::with_capacity(12 + frame.payload.len());
        buf.push(MAGIC);
        buf.push(PROTOCOL_VERSION);
        buf.push(frame.flags.0);
        buf.push(frame.frame_type as u8);
        buf.extend_from_slice(&frame.stream_id.to_le_bytes());
        buf.extend_from_slice(&(frame.payload.len() as u32).to_le_bytes());
        buf.extend_from_slice(&frame.payload);
        Ok(buf)
    }

    /// 解码一帧。数据不足时返回 `None`（需要更多字节）。
    pub fn decode(buf: &[u8]) -> Result<Option<Frame>, NsmtError> {
        if buf.len() < 12 {
            return Ok(None);
        }
        if buf[0] != MAGIC {
            return Err(NsmtError::InvalidMagic(buf[0]));
        }
        let version = buf[1];
        if version != PROTOCOL_VERSION {
            return Err(NsmtError::UnsupportedVersion(version));
        }
        let frame_type = FrameType::from_raw(buf[3])
            .ok_or_else(|| NsmtError::Protocol(format!("unknown frame type {:#x}", buf[3])))?;
        let flags = Flags(buf[2]);
        let stream_id = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let payload_len = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]) as usize;
        if payload_len > MAX_PAYLOAD_LEN as usize {
            return Err(NsmtError::FrameTooLarge(payload_len));
        }
        if buf.len() < 12 + payload_len {
            return Ok(None);
        }
        let payload = buf[12..12 + payload_len].to_vec();
        Ok(Some(Frame {
            frame_type,
            flags,
            stream_id,
            payload,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_frame() {
        let f = Frame::new(FrameType::Hello, 0, b"hello".to_vec());
        let wire = FrameCodec::encode(&f).unwrap();
        let decoded = FrameCodec::decode(&wire).unwrap().unwrap();
        assert_eq!(f, decoded);
    }

    #[test]
    fn json_payload_roundtrip() {
        let f = Frame::from_json(FrameType::MemoryRecall, 2, &serde_json::json!({
            "query": "test", "scope": "user"
        }))
        .unwrap();
        let wire = FrameCodec::encode(&f).unwrap();
        let decoded = FrameCodec::decode(&wire).unwrap().unwrap();
        let v: serde_json::Value = decoded.payload_json().unwrap();
        assert_eq!(v["query"], "test");
    }

    #[test]
    fn partial_buffer_returns_none() {
        let f = Frame::new(FrameType::Error, 1, vec![0u8; 64]);
        let wire = FrameCodec::encode(&f).unwrap();
        let cut = &wire[..wire.len() - 10];
        assert!(FrameCodec::decode(cut).unwrap().is_none());
    }
}
