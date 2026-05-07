use std::net::SocketAddr;
use std::collections::HashMap;
use serde::Deserialize;

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

// --- Upstream 定义 ---
#[derive(Deserialize, Debug)]
pub struct UpstreamConfig {
    pub servers: Vec<SocketAddr>, // 支持多台服务器
    pub load_balancing: LbMethod, // 负载均衡算法
}

#[derive(Deserialize, Debug)]
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

#[derive(Deserialize, Debug)]
pub struct HttpServerConfig {
    pub listen: SocketAddr,
    pub server_name: String, // 虚拟主机支持 (e.g., www.example.com)
    // 增加中间件/指令配置
    pub proxy_set_header: HashMap<String, String>,
    pub locations: Vec<LocationConfig>,
}

#[derive(Deserialize, Debug)]
pub struct LocationConfig {
    pub path: String,
    pub match_type: LocationMatch, // 精确匹配 / 前缀匹配
    // 动作：要么是代理，要么是静态文件
    pub action: LocationAction,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type", content = "value")]
pub enum LocationAction {
    Proxy(String), // 引用 UpstreamConfig 的 name
    Static(String), // 本地文件路径 (root)
}

#[derive(Deserialize, Debug)]
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

#[derive(Deserialize, Debug)]
pub struct StreamServerConfig {
    pub listen: SocketAddr,
    // Stream 也可以引用 Upstream，实现 TCP 负载均衡
    pub target: StreamTarget
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type", content = "value")]
pub enum StreamTarget {
    Upstream(String),         // 引用 upstreams 里的名字
    Direct(SocketAddr),       // 简单直连
}