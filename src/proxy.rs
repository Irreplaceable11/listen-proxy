use crate::config::{
    LocationAction, LocationMatch, RuntimeHttpServer,
    RuntimeLocation, RuntimeProxyHeader,
};
use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{HOST, HeaderValue};
use hyper::{HeaderMap, Request, Response, StatusCode, Uri};
use hyper_util::rt::TokioIo;
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpStream;
use tracing::{debug, warn};
use crate::upstream::RuntimeUpstream;

pub async fn handler_http(
    req: Request<Incoming>,
    server: Arc<RuntimeHttpServer>,
    upstreams: Arc<HashMap<String, RuntimeUpstream>>,
    remote_addr: SocketAddr,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, Infallible> {
    let req_path = req.uri().path();

    let match_location = match_location(req_path, &server.locations);

    // 根据location做转发或者静态文件代理
    if let Some(loc) = match_location {
        match &loc.action {
            LocationAction::Proxy(name) => {
                if let Some(upstream) = upstreams.get(name) {
                    if let Some(target) = upstream.select_server(Some(remote_addr)) {
                        return Ok(proxy_to_upstream(
                            req,
                            target,
                            &loc.proxy_set_header,
                            remote_addr,
                        )
                        .await);
                    }
                } else {
                    return Ok(error_response(StatusCode::BAD_GATEWAY, "bad gateway"));
                }
            }
            LocationAction::Static(path) => {
                return Ok(Response::new(
                    Full::new(Bytes::from(format!("static path {}", path)))
                        .map_err(|never| match never {})
                        .boxed(),
                ));
            }
        }
    }

    Ok(full_response("hello"))
}

async fn proxy_to_upstream(
    req: Request<Incoming>,
    target: SocketAddr,
    proxy_set_header: &Vec<RuntimeProxyHeader>,
    remote_addr: SocketAddr,
) -> Response<BoxBody<Bytes, hyper::Error>> {
    let method = req.method().clone();
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|v| v.as_str())
        .unwrap_or("/")
        .to_string();
    let upstream_uri = match path_and_query.parse::<Uri>() {
        Ok(uri) => uri,
        Err(err) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                format!("invalid upstream uri: {}", err),
            );
        }
    };

    debug!(
        method = %method,
        uri = %path_and_query,
        upstream = %target,
        "proxy request to upstream",
    );

    let stream = match TcpStream::connect(target).await {
        Ok(stream) => stream,
        Err(err) => {
            warn!(
                method = %method,
                uri = %path_and_query,
                upstream = %target,
                error = %err,
                "failed to connect upstream",
            );
            return error_response(
                StatusCode::BAD_GATEWAY,
                format!("failed to connect upstream {}: {}", target, err),
            );
        }
    };

    let io = TokioIo::new(stream);
    let (mut sender, conn) = match hyper::client::conn::http1::handshake(io).await {
        Ok(parts) => parts,
        Err(err) => {
            warn!(
                method = %method,
                uri = %path_and_query,
                upstream = %target,
                error = %err,
                "failed to handshake upstream",
            );
            return error_response(
                StatusCode::BAD_GATEWAY,
                format!("failed to handshake upstream {}: {}", target, err),
            );
        }
    };

    tokio::spawn(async move {
        if let Err(err) = conn.await {
            tracing::error!("upstream connection error: {}", err);
        }
    });
    let original_host = req.headers().get(HOST).cloned();
    let (mut parts, body) = req.into_parts();
    parts.uri = upstream_uri;

    let upstream_host = HeaderValue::try_from(target.to_string())
        .expect("SocketAddr should be a valid Host header value");

    apply_proxy_headers(
        &mut parts.headers,
        proxy_set_header,
        original_host,
        upstream_host,
        remote_addr,
    );

    match sender.send_request(Request::from_parts(parts, body)).await {
        Ok(response) => {
            debug!(
                method = %method,
                uri = %path_and_query,
                upstream = %target,
                status = %response.status(),
                "received upstream response",
            );
            response.map(|body| body.boxed())
        }
        Err(err) => {
            warn!(
                method = %method,
                uri = %path_and_query,
                upstream = %target,
                error = %err,
                "failed to send request to upstream",
            );
            error_response(
                StatusCode::BAD_GATEWAY,
                format!("failed to send request to upstream {}: {}", target, err),
            )
        }
    }
}

fn apply_proxy_headers(
    header_map: &mut HeaderMap,
    proxy_header: &[RuntimeProxyHeader],
    original_host: Option<HeaderValue>,
    upstream_host: HeaderValue,
    remote_addr: SocketAddr,
) {
    //默认 Host 指向 upstream；如果 proxy_set_header 里配置了 Host，会在下面被覆盖(Host为用户访问的域名)
    header_map.insert(HOST, upstream_host);
    for header in proxy_header {
        let value = if header.value == "$host" {
            match &original_host {
                Some(value) => value.clone(),
                None => continue,
            }
        } else if header.value == "$remote_addr" {
            match HeaderValue::try_from(remote_addr.ip().to_string()) {
                Ok(value) => value,
                Err(_) => continue,
            }
        } else {
            match header.value.parse() {
                Ok(value) => value,
                Err(_) => continue,
            }
        };
        header_map.insert(header.name.clone(), value);
    }
}

fn match_location<'a>(path: &str, locations: &'a [RuntimeLocation]) -> Option<&'a RuntimeLocation> {
    let mut match_location = None;
    // 查找对应的location规则
    // 1. 先检查 Exact
    if let Some(loc) = locations.iter().find(|l| l.is_exact() && path == l.path) {
        match_location = Some(loc);
    } else {
        let mut longest_prefix: Option<&RuntimeLocation> = None;
        for location_config in locations {
            match location_config.match_type {
                LocationMatch::Prefix => {
                    if path.starts_with(&location_config.path) {
                        if longest_prefix.is_none()
                            || location_config.path.len() > longest_prefix.unwrap().path.len()
                        {
                            longest_prefix = Some(location_config);
                        }
                    }
                }
                LocationMatch::Regex => {
                    if let Some(ref r) = location_config.regex {
                        if r.is_match(path) {
                            match_location = Some(location_config);
                            break;
                        }
                    }
                }
                _ => {}
            }
        }

        // 如果正则没命中，使用最长的 Prefix
        if match_location.is_none() {
            match_location = longest_prefix;
        }
    }
    match_location
}

fn full_response<T: Into<Bytes>>(body: T) -> Response<BoxBody<Bytes, hyper::Error>> {
    Response::new(
        Full::new(body.into())
            .map_err(|never| match never {})
            .boxed(),
    )
}

fn error_response<T: Into<Bytes>>(
    status: StatusCode,
    body: T,
) -> Response<BoxBody<Bytes, hyper::Error>> {
    let mut response = full_response(body);
    *response.status_mut() = status;
    response
}
