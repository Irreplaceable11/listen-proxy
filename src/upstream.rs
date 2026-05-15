use crate::config::{LbMethod, UpstreamConfig};
use ahash::RandomState;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug)]
pub struct RuntimeUpstream {
    pub servers: Vec<SocketAddr>,
    pub load_balancing: LbMethod,
    pub round_robin_index: AtomicUsize,
    pub state: RandomState,
}

impl RuntimeUpstream {
    pub fn from_config(config: UpstreamConfig) -> Self {
        Self {
            servers: config.servers,
            load_balancing: config.load_balancing,
            round_robin_index: AtomicUsize::new(0),
            state: RandomState::with_seed(0),
        }
    }

    pub fn select_server(&self, remote_addr: Option<SocketAddr>) -> Option<SocketAddr> {
        if self.servers.is_empty() {
            return None;
        }

        match self.load_balancing {
            LbMethod::RoundRobin => self.select_round_robin(),
            LbMethod::LeastConn => self.select_first(),
            LbMethod::IpHash => {
                let ip = remote_addr?.ip();
                self.select_ip_hash(ip)
            }
        }
    }

    fn select_round_robin(&self) -> Option<SocketAddr> {
        let index = self.round_robin_index.fetch_add(1, Ordering::Relaxed);
        Some(self.servers[index % self.servers.len()])
    }

    fn select_ip_hash(&self, remote_ip: IpAddr) -> Option<SocketAddr> {
        if self.servers.is_empty() {
            return None;
        }

        let hash = self.state.hash_one(remote_ip);
        let index = hash as usize % self.servers.len();

        Some(self.servers[index])
    }

    fn key_from_addr_and_ua(addr: SocketAddr, ua: &str) -> Vec<u8> {
        let ip_bytes = match addr.ip() {
            IpAddr::V4(ip) => ip.octets().to_vec(),
            IpAddr::V6(ip) => ip.octets().to_vec(),
        };

        let ua_bytes = ua.as_bytes();

        let mut key = Vec::with_capacity(ip_bytes.len() + 4 + ua_bytes.len());

        // 写入 IP
        key.extend_from_slice(&ip_bytes);

        // 写入 UA 长度（4字节，避免歧义）
        key.extend_from_slice(&(ua_bytes.len() as u32).to_be_bytes());

        // 写入 UA 内容
        key.extend_from_slice(ua_bytes);

        key
    }

    fn select_first(&self) -> Option<SocketAddr> {
        self.servers.first().copied()
    }
}
