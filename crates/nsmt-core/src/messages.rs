//! 协议消息 payload（protocol.md §5–§8）。
//!
//! 控制帧 payload 为 JSON。这里定义握手 / 心跳 / 在线列表 / 记忆 / 锁 等消息结构。

use serde::{Deserialize, Serialize};

// ── 握手 / 鉴权 / 注册 ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub user_domain: String,
    pub protocol_version: u8,
    pub client: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloAck {
    pub nonce: String,
    pub tenant_exists: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Auth {
    pub user_domain: String,
    /// 对 nonce 的 Ed25519 签名（hex）。M0 开发期不做强校验。
    pub nonce_signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    pub ticket: String,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Register {
    pub machine_id: String,
    pub agent_tag: String,
    /// P2P 直连监听地址（客户端上报）。
    #[serde(default)]
    pub peer_addr: String,
    /// 机器公钥（hex，Ed25519）。
    pub machine_pubkey: String,
    /// 对 `machine_id + "\n" + agent_tag` 的机器签名（hex）。
    pub machine_signature: String,
}

/// 在线机器信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineInfo {
    pub machine_id: String,
    pub agents: Vec<String>,
    pub addr: String,
    /// P2P 直连地址（客户端监听），为空则不可直连。
    #[serde(default)]
    pub peer_addr: String,
    /// 机器公钥（hex，Ed25519）——P2P 对等认证用（M9.1）。
    #[serde(default)]
    pub machine_pubkey: String,
    pub last_seen: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterAck {
    pub machines: Vec<MachineInfo>,
}

// ── 心跳 / 在线状态 ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Heartbeat {
    pub ts: u64,
    pub load: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnlineList {
    pub machines: Vec<MachineInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnlineDeltaKind {
    Join,
    Leave,
    AgentChange,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnlineDelta {
    pub kind: OnlineDeltaKind,
    pub machine: MachineInfo,
}

// ── 记忆 ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRecall {
    pub request_id: String,
    pub query: String,
    #[serde(default = "default_scope")]
    pub scope: String,
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    #[serde(default = "default_true")]
    pub fallback_on_timeout: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRecallResult {
    pub request_id: String,
    pub source: String,
    pub memories: Vec<MemoryHit>,
    pub latency_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryHit {
    pub content: String,
    pub fqn: String,
    pub score: f32,
    pub scope: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryCapture {
    pub request_id: String,
    pub user_content: String,
    pub assistant_content: String,
    #[serde(default = "default_scope")]
    pub scope: String,
    pub fqn: String,
    pub observed_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryCaptureResult {
    pub request_id: String,
    pub committed: bool,
    pub queued: bool,
}

// ── 文件 ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileTree {
    pub tree_hash: String,
    pub entries: Vec<FileTreeEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileTreeEntry {
    pub path: String,
    pub blob_id: String,
    pub mode: u16,
    pub size: u64,
    pub mtime_ns: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDiff {
    pub old_tree: String,
    /// 客户端不知道远端最新树时省略；服务端以最新树应答。
    #[serde(default)]
    pub new_tree: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDiffResult {
    pub changed: Vec<String>,
    pub removed: Vec<String>,
    /// 最新目录树（客户端据此拿 blob_id 拉取）。
    #[serde(default)]
    pub tree: Option<FileTree>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileGet {
    pub blob_id: String,
    pub chunk_index: Option<u64>,
}

/// 服务器对 FILE_PUT 的确认：已收到的块（供客户端断点续传）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilePutAck {
    pub blob_id: String,
    #[serde(default)]
    pub have: Vec<u64>,
    pub completed: bool,
}

/// FILE_CHUNK 的元信息（独立于二进制载荷发送前）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChunkMeta {
    pub blob_id: String,
    pub chunk_index: u64,
    pub len: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilePut {
    pub blob_id: String,
    pub total_chunks: u64,
    pub size: u64,
}

// ── 锁 ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockAcquire {
    pub path: String,
    pub requester: String,
    pub ttl_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockRenew {
    pub path: String,
    pub requester: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockRelease {
    pub path: String,
    pub requester: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockGranted {
    pub path: String,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockDenied {
    pub path: String,
    pub holder: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockNotify {
    pub path: String,
    pub event: String,
    pub holder: Option<String>,
}

// ── P2P 对等认证 / 打洞（M9.1）──

/// P2P 连接建立后的对等认证第一步：发起方自报身份。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerHello {
    pub user_domain: String,
    pub machine_id: String,
    pub agent_tag: String,
    pub machine_pubkey: String,
}

/// 对等认证第二步：对端下发 nonce，要求发起方用域密钥签名。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerAuth {
    pub nonce: String,
}

/// 对等认证第三步：发起方回签名（域密钥，同一用户域共享）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerAuthOk {
    pub machine_id: String,
    pub agent_tag: String,
    pub machine_pubkey: String,
    /// 对 nonce 的域密钥签名（hex）。
    pub signature: String,
}

/// 服务器 → owner：有人请求本机拥有的对象，提示主动打洞（NAT hole punching）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerHint {
    pub blob_id: String,
    /// requester 的外部地址（服务器观测），owner 主动直连目标。
    pub requester_addr: String,
}

// ── 错误 ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorMsg {
    pub code: String,
    pub message: String,
    pub request_id: Option<String>,
}

fn default_scope() -> String {
    "user".into()
}
fn default_limit() -> u32 {
    5
}
fn default_timeout() -> u64 {
    1500
}
fn default_true() -> bool {
    true
}
