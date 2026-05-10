# listen-proxy

一个用于学习 Rust 异步网络编程和反向代理实现的轻量级代理项目。

## 本地测试 HTTP 转发

这一节用于验证当前最小版 HTTP 反向代理是否能把请求转发到 upstream，并保留 path、query 和 body。

### 1. 启动一个 Node mock upstream

在一个终端中启动临时 upstream 服务：

```powershell
node -e "require('http').createServer((req,res)=>{let body='';req.on('data',c=>body+=c);req.on('end',()=>{res.setHeader('content-type','application/json');res.end(JSON.stringify({method:req.method,url:req.url,host:req.headers.host,body},null,2));});}).listen(8081,()=>console.log('mock upstream on 8081'))"
```

这个服务会回显它收到的请求方法、URL、Host 和请求体，方便观察代理是否转发正确。

### 2. 修改测试配置

把 `proxy-config.toml` 中的 HTTP upstream 临时改成本机 mock 服务：

```toml
[upstreams.backend_api]
servers = ["127.0.0.1:8081"]
load_balancing = "round_robin"
```

建议本地测试时把 HTTP 监听端口改成 `3000`，避免占用或权限问题：

```toml
[[http.servers]]
listen = "127.0.0.1:3000"
```

### 3. 启动 listen-proxy

另开一个终端，启动代理：

```powershell
cargo run
```

### 4. 测试 GET 请求

```powershell
curl "http://127.0.0.1:3000/api/v1/users?id=1"
```

期望看到类似结果：

```json
{
  "method": "GET",
  "url": "/api/v1/users?id=1",
  "host": "127.0.0.1:8081",
  "body": ""
}
```

这里重点确认：

- `url` 保留了原请求的 path 和 query。
- `host` 被重写成 upstream 地址。

### 5. 测试 POST 请求体

```powershell
curl -X POST "http://127.0.0.1:3000/api/v1/users?id=1" -H "content-type: application/json" -d "{\"name\":\"alice\"}"
```

期望看到类似结果：

```json
{
  "method": "POST",
  "url": "/api/v1/users?id=1",
  "host": "127.0.0.1:8081",
  "body": "{\"name\":\"alice\"}"
}
```

如果能看到 `body`，说明请求体没有被代理提前读取或丢弃，而是被原样转发到了 upstream。
