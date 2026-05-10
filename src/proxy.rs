use crate::config::{HttpServerConfig, LocationAction, LocationConfig, LocationMatch, UpstreamConfig};
use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::HOST;
use hyper::{Request, Response, StatusCode, Uri};
use hyper_util::rt::TokioIo;
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::net::TcpStream;
use tracing::{debug, warn};

pub async fn handler_http(
    req: Request<Incoming>,
    server: Arc<HttpServerConfig>,
    upstreams: Arc<HashMap<String, UpstreamConfig>>,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, Infallible> {
    let req_path = req.uri().path();

    let mut match_location = None;

    // 1. 先检查 Exact
    if let Some(loc) = server.locations.iter().find(|l| l.is_exact() && req_path == l.path) {
        match_location = Some(loc);
    } else {
        let mut longest_prefix:Option<&LocationConfig> = None;
        for location_config in &server.locations {
            match location_config.match_type {
                LocationMatch::Prefix => {
                    if req_path.starts_with(&location_config.path) {
                        if longest_prefix.is_none() || location_config.path.len() > longest_prefix.unwrap().path.len() {
                            longest_prefix = Some(location_config);
                        }
                    }
                }
                LocationMatch::Regex => {
                   if let Some(ref r) = location_config.regex {
                       if r.is_match(req_path) {
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

    if let Some(loc) = match_location {
        match &loc.action {
            LocationAction::Proxy(name) => {
                if let Some(upstream) = upstreams.get(name){
                    if let Some(&target) = upstream.servers.first() {
                        return Ok(proxy_to_upstream(req, target).await);
                    }
                } else {
                    return Ok(error_response(StatusCode::BAD_GATEWAY, "bad gateway"))
                }
            }
            LocationAction::Static(path) => {
                return Ok(Response::new(Full::new(Bytes::from(format!(
                    "static path {}",
                    path
                ))).map_err(|never| match never {}).boxed()));
            }
        }
    }

    Ok(full_response("hello"))
}

async fn proxy_to_upstream(
    req: Request<Incoming>,
    target: std::net::SocketAddr,
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

    let (mut parts, body) = req.into_parts();
    parts.uri = upstream_uri;
    parts.headers.insert(
        HOST,
        target
            .to_string()
            .parse()
            .expect("SocketAddr should be a valid Host header value"),
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

fn full_response<T: Into<Bytes>>(body: T) -> Response<BoxBody<Bytes, hyper::Error>> {
    Response::new(Full::new(body.into()).map_err(|never| match never {}).boxed())
}

fn error_response<T: Into<Bytes>>(
    status: StatusCode,
    body: T,
) -> Response<BoxBody<Bytes, hyper::Error>> {
    let mut response = full_response(body);
    *response.status_mut() = status;
    response
}
