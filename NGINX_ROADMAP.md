# listen-proxy 到类 nginx 能力的迭代规划

这份规划基于当前 `src/config.rs`、`src/main.rs`、`src/proxy.rs` 的实现状态整理。目标不是一次性复刻完整 nginx，而是把 listen-proxy 拆成一组可以运行、可以测试、可以逐步理解 Rust 异步网络编程的里程碑。

## 当前代码状态

### 已经具备的能力

1. 配置加载
   - `main.rs` 从 `proxy-config.toml` 读取配置。
   - `config.rs` 使用 `serde` 反序列化为结构体。
   - 已有 `MainConfig`、`HttpConfig`、`StreamConfig`、`UpstreamConfig`、`LocationConfig` 等核心配置模型。
   - `verify_configuration` 已经能检查 HTTP location 和 stream server 引用的 upstream 是否存在。

2. HTTP 监听
   - `main.rs` 根据 `http.servers` 启动多个 `TcpListener`。
   - 每个连接用 `tokio::spawn` 并发处理。
   - 使用 `hyper` 的 HTTP/1 server 处理请求。

3. HTTP location 匹配
   - `proxy.rs` 已实现 exact、prefix、regex 三类匹配的雏形。
   - prefix 匹配使用最长前缀规则，方向正确。

4. HTTP upstream 转发
   - `proxy.rs` 已经能连接 upstream。
   - 使用 `hyper::client::conn::http1::handshake` 将请求转发给后端。
   - 保留 path、query 和 body。
   - 当前会把 `Host` header 重写为 upstream 地址。

5. Stream 监听占位
   - `main.rs` 已经能根据 `stream.servers` 启动 TCP listener。
   - 当前接收连接后直接丢弃，还没有做双向转发。

### 当前最值得优先修正的问题

1. 静态文件还只是返回 `"static path ..."`，没有真正读取文件。
2. upstream 当前只取 `servers.first()`，还没有负载均衡。
3. `proxy_set_header` 已经在配置中定义，但请求转发时还没有应用。
4. stream TCP proxy 还没有实现。
5. 错误类型目前主要依赖 `anyhow` 和字符串，需要逐步引入领域错误。
6. regex 编译逻辑会给所有 location 编译正则，即使它不是 regex location。
7. 连接超时、读写超时、请求体大小限制、优雅关闭还没有形成完整机制。
8. 代码中运行时状态还比较少，后续 least_conn、ip_hash、健康检查都需要专门的 runtime state。

## 总体演进路线

建议按下面顺序迭代：

1. 稳定当前 HTTP reverse proxy 的最小闭环。
2. 补齐 nginx 最核心的 location、header、static、upstream 能力。
3. 实现 stream TCP proxy。
4. 加入超时、错误恢复、日志、指标等生产运行基础。
5. 设计配置热重载和优雅关闭。
6. 再考虑 HTTPS、HTTP/2、缓存、压缩、限流等高级能力。

每一步都应该有一个能用 `cargo test` 或本地 `curl` 验证的小目标。

## 第一阶段：让当前 HTTP 代理更正确

### 1.1 修正 location 正则编译

目的：只有 `match_type = "regex"` 的 location 才应该编译 regex。

建议改动：

- 在配置加载后遍历 location。
- 如果 `loc.match_type == LocationMatch::Regex`，再执行 `Regex::new(&loc.path)`。
- prefix 和 exact location 的 `regex` 保持 `None`。

涉及 Rust 知识点：

- 可变借用 `iter_mut()`。
- enum 匹配。
- `Result` 错误向上传递，而不是在配置阶段直接 `panic!`。

验收方式：

- prefix location 中写普通路径 `/api/v1` 不应该被当成正则编译。
- regex location 写非法正则时，启动阶段返回清晰错误。

### 1.2 抽出 location 匹配函数并加单元测试

目的：location 匹配是代理的核心规则，应该独立测试。

建议新增函数：

```rust
fn match_location<'a>(path: &str, locations: &'a [LocationConfig]) -> Option<&'a LocationConfig>
```

测试覆盖：

- exact 优先于 prefix。
- prefix 选择最长前缀。
- regex 能命中。
- 没有任何匹配时返回 `None`。

涉及 Rust 知识点：

- 生命周期 `'a`：返回的 location 借用自 `locations`。
- 不移动配置，只借用配置。
- 单元测试组织方式。

### 1.3 应用 `proxy_set_header`

目的：让配置中的 header 重写真正参与转发。

第一版只支持几个变量：

- `$host`：原始请求 Host。
- `$remote_addr`：客户端地址，当前 handler 还拿不到，需要后续从连接层传入。
- `$proxy_add_x_forwarded_for`：旧的 `X-Forwarded-For` 加上客户端 IP。

建议分两步做：

1. 先支持固定字符串 header 和 `$host`。
2. 再调整 `handler_http` 参数，把客户端地址从 `listen_http` 传入，用于 `$remote_addr` 和 `$proxy_add_x_forwarded_for`。

涉及 Rust 知识点：

- `HeaderName`、`HeaderValue` 的 fallible parse。
- 借用 request headers 后再修改 parts headers 时，要注意所有权拆分顺序。
- 配置值到运行时行为的映射。

验收方式：

- mock upstream 能看到配置里设置的 header。
- 非法 header name/value 在启动时或请求时有明确错误策略。

## 第二阶段：补齐 upstream 和负载均衡

### 2.1 引入运行时 upstream state

目的：配置是静态的，但负载均衡需要运行时状态。

建议新增模块：

- `src/upstream.rs`

核心结构：

```rust
pub struct UpstreamRuntime {
    config: UpstreamConfig,
    state: UpstreamState,
}
```

第一版可以先支持 round robin：

- 使用 `AtomicUsize` 记录下一个下标。
- 每次请求选择 `servers[index % servers.len()]`。

涉及 Rust 知识点：

- `Arc` 共享只读配置。
- `AtomicUsize` 无锁计数。
- 配置结构和运行时结构分离。

验收方式：

- 配置两个 mock upstream。
- 连续请求能轮流命中两个端口。

### 2.2 实现 `least_conn`

目的：学习共享状态和并发计数。

建议：

- 每个 upstream server 维护当前连接数。
- 请求开始时加一，结束时减一。
- 用 RAII guard 保证异常返回时也能减一。

涉及 Rust 知识点：

- `Drop` 自动释放资源。
- `Arc` + atomic 计数。
- 请求生命周期和连接生命周期的边界。

需要思考：

- 这里统计的是“正在处理的请求数”，还是“打开的 TCP 连接数”？
- HTTP keep-alive 后，一个连接可能承载多个请求，计数放在哪里更合适？

### 2.3 实现 `ip_hash`

目的：同一个客户端尽量落到同一个 upstream。

建议：

- 从客户端 IP 计算 hash。
- `hash % servers.len()` 选择 upstream。
- 后续如果健康检查发现某台不可用，需要跳过不可用节点。

涉及 Rust 知识点：

- hash 计算。
- 客户端地址如何从 listener 传递到请求 handler。
- 负载均衡策略 trait 设计。

## 第三阶段：真正实现静态文件服务

### 3.1 支持 static root

目的：让 `LocationAction::Static(path)` 真正返回文件内容。

建议第一版：

- location path `/assets` 映射到 root `/var/www/html/assets`。
- 请求 `/assets/app.js` 映射到 `/var/www/html/assets/app.js`。
- 使用 `tokio::fs::read` 异步读取文件。

必须注意：

- 防止路径穿越，例如 `/assets/../secret.txt`。
- 不要直接字符串拼接路径，使用 `Path` / `PathBuf`。

涉及 Rust 知识点：

- `PathBuf` 和路径规范化。
- 异步文件 IO。
- `Option` / `Result` 组合处理。

验收方式：

- 文件存在返回 200。
- 文件不存在返回 404。
- 目录穿越返回 403 或 404。

### 3.2 支持 MIME type

目的：让浏览器能正确识别静态资源类型。

建议：

- 第一版根据扩展名手写少量映射：html、css、js、json、png、jpg、svg。
- 后续可以考虑引入 `mime_guess`。

涉及 Rust 知识点：

- 何时引入 crate，何时先保持简单。
- response header 构造。

### 3.3 支持 index 文件和目录处理

目的：接近 nginx 的基本静态站点能力。

建议配置：

```toml
index = ["index.html", "index.htm"]
```

验收方式：

- 请求 `/` 能返回 index。
- 请求目录时按 index 顺序查找。

## 第四阶段：实现 stream TCP proxy

### 4.1 Direct TCP 双向转发

目的：让 `StreamTarget::Direct` 真正代理 TCP 流量。

实现思路：

1. listener 接收 client socket。
2. 连接 target socket。
3. 使用 `tokio::io::copy_bidirectional` 做双向拷贝。

涉及 Rust 知识点：

- `TcpStream` 的所有权。
- 双向复制和连接关闭。
- `copy_bidirectional` 返回两个方向传输的字节数。

验收方式：

- 用本地 echo server 或数据库 mock 端口验证。
- client 发数据，target 收到并返回。

### 4.2 Stream upstream 负载均衡

目的：stream 也能复用 upstream 配置。

建议：

- 让 stream proxy 使用第二阶段的 `UpstreamRuntime`。
- Direct target 直接连接。
- Upstream target 调用统一的选择器。

涉及 Rust 知识点：

- 共享模块设计。
- HTTP 和 stream 复用 upstream 选择逻辑。

### 4.3 Stream 超时和错误恢复

建议加入：

- connect timeout。
- idle timeout。
- 最大连接数。
- 连接失败时尝试下一个 upstream。

## 第五阶段：超时、限制和错误处理

### 5.1 引入代理超时配置

建议配置项：

```toml
connect_timeout_ms = 3000
read_timeout_ms = 30000
write_timeout_ms = 30000
```

可以放在：

- 全局默认。
- http server 覆盖。
- location 覆盖。
- stream server 覆盖。

第一版先放在全局或 server 级别，避免配置层级过早复杂。

涉及 Rust 知识点：

- `tokio::time::timeout`。
- 超时错误和 IO 错误的区分。
- 默认配置值 `#[serde(default)]`。

### 5.2 请求体大小限制

目的：避免大 body 占用过多资源。

建议：

- 对 HTTP 请求设置 `client_max_body_size`。
- 第一版可以在 body 转发前用 hyper body size hint 或读取时限制。

需要权衡：

- 完全流式转发更节省内存。
- 为了限制 body，有时需要包一层 body adapter。

### 5.3 使用 `thiserror` 定义领域错误

建议新增：

- `src/error.rs`

错误类型示例：

```rust
pub enum ProxyError {
    Config(String),
    UpstreamUnavailable(String),
    InvalidHeader(String),
    StaticFileForbidden,
}
```

原则：

- `main` 入口层可以继续用 `anyhow::Result`。
- config、proxy、upstream 模块内部使用清晰的领域错误。

涉及 Rust 知识点：

- `thiserror::Error`。
- `?` 自动转换。
- 应用错误和协议响应之间的映射。

## 第六阶段：日志、可观测性和调试能力

### 6.1 结构化 access log

目的：像 nginx access log 一样记录请求结果。

建议记录：

- remote_addr
- method
- path
- status
- upstream
- duration_ms
- request_id

涉及 Rust 知识点：

- `tracing` span。
- 请求开始和结束的生命周期。
- `Instant` 计时。

### 6.2 request id

建议：

- 如果请求已有 `X-Request-Id`，沿用。
- 否则生成一个新的。
- 转发给 upstream。

可以先用递增 atomic id，后续再考虑 `uuid`。

### 6.3 metrics 预留

后续可以暴露 `/metrics`：

- 总请求数。
- 状态码分布。
- upstream 失败次数。
- 当前连接数。

学习阶段可以先不引入 Prometheus crate，先维护结构化 counters。

## 第七阶段：配置系统升级

### 7.1 配置默认值和校验

建议：

- 对可选字段使用 `#[serde(default)]`。
- 启动时统一 validate。
- 错误信息包含字段路径，例如 `http.servers[0].locations[1].path`。

重点校验：

- upstream servers 不能为空。
- listen 地址不能重复。
- regex location 必须能编译。
- static root 必须是允许访问的路径。
- header 名称必须合法。

### 7.2 配置热重载

目标：接近 nginx `reload` 的体验。

第一版设计：

- 收到信号后重新读取配置。
- validate 新配置。
- 构建新的 runtime config。
- 用 `ArcSwap` 或 `Arc<RwLock<RuntimeConfig>>` 替换当前配置。

推荐学习顺序：

1. 先实现手动 reload 函数和测试。
2. 再绑定信号。
3. 最后处理 listener 端口变化。

需要思考：

- 已经建立的连接应该继续使用旧配置，还是切到新配置？
- 新配置监听端口变化时，旧 listener 怎么关闭？

## 第八阶段：优雅关闭和连接生命周期

### 8.1 统一 shutdown signal

当前 `main.rs` 已经等待 `ctrl_c`，但 listener task 不会被统一管理。

建议：

- 使用 `tokio::sync::broadcast` 分发 shutdown 信号。
- listener loop 收到信号后停止 accept。
- 已有连接允许在超时时间内完成。

涉及 Rust 知识点：

- `tokio::select!`。
- channel。
- task join handle 管理。

### 8.2 连接追踪

建议：

- 维护 active connection count。
- shutdown 时等待连接数归零或超时。

这会帮助理解：

- 谁持有连接所有权？
- 什么时候连接真正释放？
- task 取消会不会丢数据？

## 第九阶段：HTTP 能力增强

### 9.1 HTTP keep-alive 和 upstream 连接池

当前每个请求都重新连接 upstream。

后续方向：

- 使用更高层的 hyper client。
- 引入连接池。
- 复用 upstream TCP 连接。

这一步复杂度较高，建议在基础代理稳定后再做。

### 9.2 WebSocket upgrade

nginx 常见能力之一是代理 WebSocket。

需要支持：

- `Connection: upgrade`
- `Upgrade: websocket`
- upgrade 后切换到字节流双向转发。

涉及 Rust 知识点：

- HTTP upgrade。
- 从 HTTP 连接切换到底层 IO。

### 9.3 压缩和缓存

建议放到后期：

- gzip / br response compression。
- 静态文件缓存 header。
- proxy cache。

这些功能会带来较多 HTTP 细节，不适合太早加入。

## 第十阶段：TLS、HTTP/2 和安全能力

### 10.1 TLS termination

目标：listen-proxy 自己接收 HTTPS。

建议 crate：

- `rustls`
- `tokio-rustls`

配置示例方向：

```toml
tls_cert = "cert.pem"
tls_key = "key.pem"
```

### 10.2 HTTP/2

方向：

- server 侧支持 HTTP/2。
- upstream 侧按需支持 HTTP/1 或 HTTP/2。

建议等 HTTP/1 代理、stream、配置热重载稳定后再做。

### 10.3 基础安全能力

可以逐步加入：

- allow/deny IP。
- rate limit。
- basic auth。
- header size 限制。
- request timeout。

## 推荐近期任务清单

### 第 1 个小版本：HTTP 代理质量修正

目标：当前 HTTP proxy 行为更正确、更容易测试。

任务：

1. 只给 regex location 编译正则。
2. 抽出 `match_location` 并补单元测试。
3. 应用 `proxy_set_header` 的固定值和 `$host`。
4. 给 upstream 连接加 connect timeout。
5. 整理 `proxy.rs` 中的中文注释编码问题。

验收：

- `cargo test` 通过。
- `curl GET/POST` 仍然能转发到 mock upstream。
- mock upstream 能看到配置 header。

### 第 2 个小版本：Round robin upstream

目标：upstream 不再永远选第一台。

任务：

1. 新增 `upstream.rs`。
2. 构建 `HashMap<String, UpstreamRuntime>`。
3. 实现 round robin。
4. HTTP proxy 使用 runtime selector。
5. 为 selector 写单元测试。

验收：

- 两个 mock upstream 轮流收到请求。

### 第 3 个小版本：静态文件

目标：`LocationAction::Static` 真正能服务文件。

任务：

1. 实现 root + request path 映射。
2. 防止 `..` 路径穿越。
3. 文件不存在返回 404。
4. 设置基础 `Content-Type`。
5. 增加静态文件路径解析测试。

验收：

- 浏览器能打开一个简单 html/css/js 页面。

### 第 4 个小版本：Stream proxy

目标：TCP 代理能真实转发数据。

任务：

1. 实现 `StreamTarget::Direct`。
2. 复用 upstream round robin 实现 `StreamTarget::Upstream`。
3. 加 connect timeout。
4. 记录双向转发字节数。

验收：

- 本地 echo server 测试通过。
- stream upstream 多节点能轮询。

### 第 5 个小版本：优雅关闭

目标：Ctrl+C 时不粗暴丢弃所有任务。

任务：

1. main 创建 shutdown channel。
2. HTTP listener 和 stream listener 使用 `tokio::select!`。
3. 连接 task 记录 active count。
4. shutdown 时等待一小段时间。

验收：

- Ctrl+C 后日志显示停止接收新连接。
- 已有请求可以完成或超时退出。

## 建议的模块拆分

当前只有三个源文件，后面功能会变多，建议逐步拆成：

```text
src/
  main.rs          # 应用入口、信号、任务启动
  config.rs        # 配置结构、默认值、校验
  error.rs         # 领域错误
  proxy.rs         # HTTP handler 的主流程
  location.rs      # location 匹配规则
  upstream.rs      # upstream runtime 和负载均衡
  static_file.rs   # 静态文件路径解析和响应构造
  stream.rs        # TCP stream proxy
  shutdown.rs      # 优雅关闭辅助结构
```

拆分原则：

- 先有测试压力，再拆文件。
- 每个模块负责一个稳定概念。
- 不为了“看起来架构完整”提前抽象。

## 学习路线对应 Rust 知识点

1. location 匹配
   - 借用、生命周期、enum 匹配、单元测试。

2. upstream runtime
   - `Arc`、atomic、共享状态、trait 设计。

3. 静态文件
   - `PathBuf`、异步文件 IO、错误映射。

4. stream proxy
   - `TcpStream` 所有权、双向 IO、连接生命周期。

5. 超时和关闭
   - `tokio::select!`、channel、task 管理、取消安全。

6. 配置热重载
   - 不可变配置、运行时状态替换、并发读写。

## 关键设计问题

后续每做一个功能，都可以先问这几个问题：

1. 这个连接的所有权应该由谁持有？
2. 这个错误应该转成 HTTP 响应，还是向上传递让 listener 记录？
3. 配置是静态数据，还是需要运行时状态？
4. 这个功能会不会影响已有连接？
5. 如果 upstream 失败，是立即返回 502，还是尝试下一个节点？
6. 这里需要共享状态吗？如果需要，能不能用 atomic，还是必须用 lock？
7. 这个行为能不能用一个小测试固定下来？

## 不建议近期做的事情

这些功能很有价值，但建议先放后面：

1. 完整 nginx 配置语法解析。
2. HTTP/2 和 TLS。
3. proxy cache。
4. gzip / brotli 压缩。
5. Lua 插件或动态脚本。
6. 完整管理 API。
7. 多 worker process 模型。

原因是它们会把学习重点从 Rust 异步代理核心，转移到大量协议细节和工程复杂度上。当前更适合先把 HTTP proxy、upstream、static、stream、shutdown 这几条主线跑通。

## 下一步建议

最建议马上开始的是“第 1 个小版本：HTTP 代理质量修正”。它改动范围小，但会显著提高后续扩展的稳定性。

推荐顺序：

1. 先改 regex 编译逻辑。
2. 再抽 `match_location`。
3. 然后补测试。
4. 最后实现 `proxy_set_header` 的第一版。

这一步完成后，listen-proxy 的 HTTP 路由和转发基础就比较稳了，后面接 upstream runtime 和 stream proxy 会顺很多。
