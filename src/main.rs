use std::collections::HashMap;
use std::fs;
use std::sync::Arc;

use anyhow::Result;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use regex::Regex;
use time::format_description;
use time::macros::offset;
use tracing::{debug, error, info};
use tracing_subscriber::fmt;

use crate::config::{HttpServerConfig, MainConfig, StreamServerConfig, UpstreamConfig};

mod config;
mod proxy;

#[tokio::main]
async fn main() -> Result<()> {
    init_log().await;

    let str_content = fs::read_to_string("proxy-config.toml")?;

    let mut config = match toml::from_str::<MainConfig>(&str_content) {
        Ok(config) => {
            debug!("config parsed successfully");
            debug!("{:#?}", config);
            config
        }
        Err(err) => {
            debug!("failed to parse config: {}", err);
            panic!("failed to parse config: {}", err);
        }
    };

    config.verify_configuration()?;

    for http in config.http.servers.iter_mut() {
        for loc in http.locations.iter_mut() {
            let r = Regex::new(&loc.path)
                .unwrap_or_else(|_| panic!("Invalid regex in config: {}", loc.path));
            loc.regex = Some(r);
        }
    }

    let MainConfig {
        upstreams,
        http,
        stream,
        ..
    } = config;
    let upstreams = Arc::new(upstreams);

    for server in http.servers {
        let server = Arc::new(server);
        let upstreams = Arc::clone(&upstreams);

        tokio::spawn(async move {
            if let Err(err) = listen_http(server, upstreams).await {
                error!("http listener stopped: {}", err);
            }
        });
    }

    for server in stream.servers {
        tokio::spawn(async move {
            if let Err(err) = listen_stream(server).await {
                error!("stream listener stopped: {}", err);
            }
        });
    }

    tokio::signal::ctrl_c().await?;
    info!("shutdown signal received");

    Ok(())
}

async fn init_log() {
    let timer_format = format_description::parse(
        "[year]-[month padding:zero]-[day padding:zero] [hour]:[minute]:[second]",
    )
    .expect("invalid time format");
    let timer = fmt::time::OffsetTime::new(offset!(+8), timer_format);
    tracing_subscriber::fmt().with_timer(timer).init();
}

async fn listen_http(
    server: Arc<HttpServerConfig>,
    upstreams: Arc<HashMap<String, UpstreamConfig>>,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(server.listen).await?;
    info!("http listening on {}", server.listen);

    loop {
        let (socket, addr) = listener.accept().await?;
        info!("http client connected: {}", addr);

        let io = TokioIo::new(socket);
        let server = Arc::clone(&server);
        let upstreams = Arc::clone(&upstreams);

        tokio::task::spawn(async move {
            let service = service_fn(move |req| {
                proxy::handler_http(req, Arc::clone(&server), Arc::clone(&upstreams))
            });

            if let Err(err) = http1::Builder::new().serve_connection(io, service).await {
                error!("Error serving connection: {:?}", err);
            }
        });
    }
}

async fn listen_stream(server: StreamServerConfig) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(server.listen).await?;
    info!("stream listening on {}", server.listen);

    loop {
        let (socket, addr) = listener.accept().await?;
        info!("stream client connected: {}", addr);

        tokio::spawn(async move {
            drop(socket);
        });
    }
}
