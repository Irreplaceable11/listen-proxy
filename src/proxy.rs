use crate::config::{HttpServerConfig, LocationAction, LocationConfig, LocationMatch, UpstreamConfig};
use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Request, Response};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;

pub async fn handler_http(
    req: Request<Incoming>,
    server: Arc<HttpServerConfig>,
    upstreams: Arc<HashMap<String, UpstreamConfig>>,
) -> Result<Response<Full<Bytes>>, Infallible> {
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
                        return Ok(Response::new(Full::new(Bytes::from(format!(
                            "proxy to {}",
                            target
                        )))));
                    }
                };
            }
            LocationAction::Static(path) => {
                return Ok(Response::new(Full::new(Bytes::from(format!(
                    "static path {}",
                    path
                )))));
            }
        }
    }

    Ok(Response::new(Full::new(Bytes::from_static(b"hello"))))
}