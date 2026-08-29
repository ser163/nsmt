//! NSMT (Yggdrasil) 核心库。
//!
//! 实现协议 v0.1 的基础类型：
//! - 身份与命名空间（FQN / MachineId / AgentTag / 密钥）
//! - 二进制帧编解码（Frame / FrameCodec）
//! - 协议错误码
//!
//! 对应文档：`protocol/protocol.md`（本项目最重要文档）。

pub mod crypto;
pub mod error;
pub mod frame;
pub mod identity;
pub mod messages;
pub mod wire;

pub use crypto::KeyPair;
pub use error::{ErrorCode, NsmtError};
pub use frame::{Flags, Frame, FrameCodec, FrameType};
pub use identity::{AgentTag, Fqn, MachineId, UserDomain};
pub use wire::FrameStream;
