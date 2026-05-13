use std::cmp::PartialEq;
use std::collections::HashMap;
use std::net::SocketAddr;

use anyhow::{Context, Result, anyhow};
use hyper::header::HeaderName;
use regex::Regex;
use serde::Deserialize;

use crate::upstream::RuntimeUpstream;

// 1. 全局配置 (对应 nginx.conf 的顶层指令)
#[derive(Deserialize, Debug)]
pub struct MainConfig {
    pub worker_processes: u32,
    pub error_log: String,
    // 2. 定义上游服务器组 (解耦 Location 和 具体IP)
    pub upstreams: HashMap<String, UpstreamConfig>,
    // 3. 包含 HTTP 和 Stream 块
    pub http: HttpConfig,
    pub stream: StreamConfig,
}

impl MainConfig {
    pub fn verify_configuration(&self) -> Result<()> {
        let mut res = Ok(());
        for http_config in &self.http.servers {
            http_config.locations.iter().for_each(|location| {
                if let LocationAction::Proxy(name) = &location.action {
                    if !&self.upstreams.contains_key(name) {
                        res = Err(anyhow!(format!("{} not upstream", name)));
                    }
                }
            })
        }
        for stream_config in &self.stream.servers {
            if let StreamTarget::Upstream(name) = &stream_config.target {
                if !&self.upstreams.contains_key(name) {
                    res = Err(anyhow!(format!("{} not upstream", name)));
                }
            }
        }
        res
    }
}

// --- Upstream 定义 ---
#[derive(Deserialize, Debug)]
pub struct UpstreamConfig {
    pub servers: Vec<SocketAddr>, // 支持多台服务器
    pub load_balancing: LbMethod, // 负载均衡算法
}

#[derive(Deserialize, Debug, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum LbMethod {
    RoundRobin,
    LeastConn,
    IpHash,
}

// --- HTTP 块 ---
#[derive(Deserialize, Debug)]
pub struct HttpConfig {
    pub include_mimes: bool,
    pub servers: Vec<HttpServerConfig>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct HttpServerConfig {
    pub listen: SocketAddr,
    pub server_name: String, // 虚拟主机支持 (e.g., www.example.com)
    // 增加中间件/指令配置
    pub proxy_set_header: HashMap<String, String>,
    pub locations: Vec<LocationConfig>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct LocationConfig {
    pub path: String,
    pub match_type: LocationMatch, // 精确匹配 / 前缀匹配
    // 动作：要么是代理，要么是静态文件
    pub action: LocationAction,

    #[serde(default)]
    pub proxy_set_header: HashMap<String, String>,
}

impl LocationConfig {
    pub fn is_exact(&self) -> bool {
        self.match_type == LocationMatch::Exact
    }

    pub fn is_regex(&self) -> bool {
        self.match_type == LocationMatch::Regex
    }

    pub fn is_prefix(&self) -> bool {
        self.match_type == LocationMatch::Prefix
    }
}

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "type", content = "value")]
pub enum LocationAction {
    Proxy(String),  // 引用 UpstreamConfig 的 name
    Static(String), // 本地文件路径 (root)
}

#[derive(Deserialize, Debug, PartialEq, Clone)]
#[serde(rename_all = "snake_case")]
pub enum LocationMatch {
    Prefix,
    Exact,
    Regex,
}

// --- Stream 块 ---
#[derive(Deserialize, Debug)]
pub struct StreamConfig {
    pub servers: Vec<StreamServerConfig>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct StreamServerConfig {
    pub listen: SocketAddr,
    // Stream 也可以引用 Upstream，实现 TCP 负载均衡
    pub target: StreamTarget,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "type", content = "value")]
pub enum StreamTarget {
    Upstream(String),   // 引用 upstreams 里的名字
    Direct(SocketAddr), // 简单直连
}
// ------------------ runtime ------------------------------------

#[derive(Debug)]
pub struct RuntimeConfig {
    pub worker_processes: u32,
    pub error_log: String,
    pub upstreams: HashMap<String, RuntimeUpstream>,
    pub http: RuntimeHttpConfig,
    pub stream: RuntimeStreamConfig,
}

impl TryFrom<MainConfig> for RuntimeConfig {
    type Error = anyhow::Error;

    fn try_from(config: MainConfig) -> Result<Self> {
        config.verify_configuration()?;

        Ok(Self {
            worker_processes: config.worker_processes,
            error_log: config.error_log,
            upstreams: config
                .upstreams
                .into_iter()
                .map(|(name, upstream)| (name, RuntimeUpstream::from_config(upstream)))
                .collect(),
            http: RuntimeHttpConfig::from_config(config.http)?,
            stream: RuntimeStreamConfig::from_config(config.stream),
        })
    }
}

#[derive(Debug)]
pub struct RuntimeHttpConfig {
    pub include_mimes: bool,
    pub servers: Vec<RuntimeHttpServer>,
}

impl RuntimeHttpConfig {
    pub fn from_config(config: HttpConfig) -> Result<Self> {
        let servers = config
            .servers
            .into_iter()
            .map(RuntimeHttpServer::from_config)
            .collect::<Result<Vec<_>>>()?;

        Ok(Self {
            include_mimes: config.include_mimes,
            servers,
        })
    }
}

#[derive(Debug)]
pub struct RuntimeHttpServer {
    pub listen: SocketAddr,
    pub server_name: String,
    pub locations: Vec<RuntimeLocation>,
}

impl RuntimeHttpServer {
    pub fn from_config(config: HttpServerConfig) -> Result<Self> {
        let locations = config
            .locations
            .iter()
            .map(|location| RuntimeLocation::from_config(&config.proxy_set_header, location))
            .collect::<Result<Vec<_>>>()?;

        Ok(Self {
            listen: config.listen,
            server_name: config.server_name,
            locations,
        })
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeLocation {
    pub path: String,
    pub match_type: LocationMatch,
    pub action: LocationAction,
    pub proxy_set_header: Vec<RuntimeProxyHeader>,
    pub regex: Option<Regex>,
}

impl RuntimeLocation {

    pub fn is_exact(&self) -> bool {
        self.match_type == LocationMatch::Exact
    }

    pub fn is_regex(&self) -> bool {
        self.match_type == LocationMatch::Regex
    }

    pub fn is_prefix(&self) -> bool {
        self.match_type == LocationMatch::Prefix
    }

    pub fn from_config(
        server_headers: &HashMap<String, String>,
        location: &LocationConfig,
    ) -> Result<Self> {
        let mut proxy_set_header = location.proxy_set_header.clone();

        for (key, val) in server_headers {
            proxy_set_header
                .entry(key.clone())
                .or_insert_with(|| val.clone());
        }

        let regex = if location.is_regex() {
            Some(
                Regex::new(&location.path)
                    .with_context(|| format!("invalid location regex: {}", location.path))?,
            )
        } else {
            None
        };

        let proxy_set_header = proxy_set_header
            .into_iter()
            .map(|(name, value)| RuntimeProxyHeader::new(name, value))
            .collect::<Result<Vec<_>>>()?;

        Ok(Self {
            path: location.path.clone(),
            match_type: location.match_type.clone(),
            action: location.action.clone(),
            proxy_set_header,
            regex,
        })
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeProxyHeader {
    pub name: HeaderName,
    pub value: String,
}

impl RuntimeProxyHeader {
    fn new(name: String, value: String) -> Result<Self> {
        let name = name
            .parse::<HeaderName>()
            .with_context(|| format!("invalid proxy_set_header name: {}", name))?;

        Ok(Self { name, value })
    }
}

#[derive(Debug)]
pub struct RuntimeStreamConfig {
    pub servers: Vec<RuntimeStreamServer>,
}

impl RuntimeStreamConfig {
    pub fn from_config(config: StreamConfig) -> Self {
        Self {
            servers: config
                .servers
                .into_iter()
                .map(RuntimeStreamServer::from_config)
                .collect(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeStreamServer {
    pub listen: SocketAddr,
    pub target: StreamTarget,
}

impl RuntimeStreamServer {
    fn from_config(config: StreamServerConfig) -> Self {
        Self {
            listen: config.listen,
            target: config.target,
        }
    }
}
