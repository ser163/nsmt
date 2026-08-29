//! 在线注册表（protocol.md §8）。
//!
//! 租户维度：`t/<user_domain>/online/<machine_id>`。
//! 每个租户维护在线机器表 + 订阅者广播通道。

use nsmt_core::frame::{Frame, FrameType};
use nsmt_core::messages::{MachineInfo, OnlineDelta, OnlineDeltaKind, OnlineList};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, RwLock};

pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
pub const OFFLINE_AFTER: Duration = Duration::from_secs(30);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Default)]
struct Tenant {
    machines: HashMap<String, MachineInfo>,
    subscribers: Vec<mpsc::UnboundedSender<Frame>>,
}

/// 租户注册表。
#[derive(Default)]
pub struct Registry {
    inner: RwLock<HashMap<String, Tenant>>,
}

impl Registry {
    /// 注册机器 + 订阅该租户广播。返回当前在线快照。
    pub async fn register(
        &self,
        user_domain: &str,
        info: MachineInfo,
    ) -> Vec<MachineInfo> {
        let mut g = self.inner.write().await;
        let tenant = g.entry(user_domain.to_string()).or_default();
        tenant.machines.insert(info.machine_id.clone(), info);
        tenant.machines.values().cloned().collect()
    }

    /// 订阅租户广播，返回（当前快照, 发送端, 接收通道）。
    pub async fn subscribe(
        &self,
        user_domain: &str,
    ) -> (Vec<MachineInfo>, mpsc::UnboundedSender<Frame>, mpsc::UnboundedReceiver<Frame>) {
        let mut g = self.inner.write().await;
        let tenant = g.entry(user_domain.to_string()).or_default();
        let (tx, rx) = mpsc::unbounded_channel();
        tenant.subscribers.push(tx.clone());
        (tenant.machines.values().cloned().collect(), tx, rx)
    }

    pub async fn unsubscribe(&self, user_domain: &str, tx: &mpsc::UnboundedSender<Frame>) {
        let mut g = self.inner.write().await;
        if let Some(t) = g.get_mut(user_domain) {
            t.subscribers.retain(|s| !s.same_channel(tx));
        }
    }

    /// 心跳刷新。
    pub async fn heartbeat(&self, user_domain: &str, machine_id: &str) {
        let mut g = self.inner.write().await;
        if let Some(t) = g.get_mut(user_domain) {
            if let Some(m) = t.machines.get_mut(machine_id) {
                m.last_seen = now_ms();
            }
        }
    }

    /// 广播一帧给租户所有订阅者。
    pub async fn broadcast(&self, user_domain: &str, frame: Frame) {
        let g = self.inner.read().await;
        if let Some(t) = g.get(user_domain) {
            for tx in &t.subscribers {
                let _ = tx.send(frame.clone());
            }
        }
    }

    /// 广播一帧，但排除某个订阅者（如刚注册的客户端自身）。
    pub async fn broadcast_except(
        &self,
        user_domain: &str,
        frame: Frame,
        except: &mpsc::UnboundedSender<Frame>,
    ) {
        let g = self.inner.read().await;
        if let Some(t) = g.get(user_domain) {
            for tx in &t.subscribers {
                if !tx.same_channel(except) {
                    let _ = tx.send(frame.clone());
                }
            }
        }
    }

    /// 定期清理离线机器并广播 leave。
    pub async fn prune_loop(self: Arc<Self>) {
        let mut ticker = tokio::time::interval(Duration::from_secs(10));
        loop {
            ticker.tick().await;
            let stale = {
                let g = self.inner.read().await;
                let mut stale = Vec::new();
                for (domain, t) in g.iter() {
                    let cutoff = now_ms().saturating_sub(OFFLINE_AFTER.as_millis() as u64);
                    for m in t.machines.values() {
                        if m.last_seen < cutoff {
                            stale.push((domain.clone(), m.clone()));
                        }
                    }
                }
                stale
            };
            for (domain, info) in stale {
                let mut g = self.inner.write().await;
                if let Some(t) = g.get_mut(&domain) {
                    if let Some(m) = t.machines.get(&info.machine_id) {
                        if m.last_seen < now_ms().saturating_sub(OFFLINE_AFTER.as_millis() as u64)
                        {
                            t.machines.remove(&info.machine_id);
                        }
                    }
                }
                drop(g);
                let delta = OnlineDelta {
                    kind: OnlineDeltaKind::Leave,
                    machine: info,
                };
                let frame = Frame::from_json(FrameType::OnlineDelta, 0, &delta).unwrap_or_else(|_| {
                    Frame::new(FrameType::Error, 0, Vec::new())
                });
                self.broadcast(&domain, frame).await;
            }
        }
    }

    /// 查某机器在线信息。
    pub async fn online_machine(&self, user_domain: &str, machine_id: &str) -> Option<MachineInfo> {
        let g = self.inner.read().await;
        g.get(user_domain)
            .and_then(|t| t.machines.get(machine_id))
            .cloned()
    }

    /// 全量在线快照：`tenant -> machines`（控制 API 用）。
    pub async fn all_online(&self) -> std::collections::HashMap<String, Vec<MachineInfo>> {
        let g = self.inner.read().await;
        g.iter()
            .map(|(d, t)| (d.clone(), t.machines.values().cloned().collect()))
            .collect()
    }

    /// 构造 ONLINE_LIST 帧。
    pub async fn online_list_frame(&self, user_domain: &str) -> Frame {
        let g = self.inner.read().await;
        let machines = g
            .get(user_domain)
            .map(|t| t.machines.values().cloned().collect())
            .unwrap_or_default();
        let msg = OnlineList { machines };
        Frame::from_json(FrameType::OnlineList, 0, &msg).unwrap_or_else(|_| {
            Frame::new(FrameType::Error, 0, Vec::new())
        })
    }
}
