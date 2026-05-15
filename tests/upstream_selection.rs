use listen_proxy::config::{LbMethod, UpstreamConfig};
use listen_proxy::upstream::RuntimeUpstream;
use std::net::SocketAddr;

fn addr(value: &str) -> SocketAddr {
    value.parse().expect("test socket addr should be valid")
}

fn upstream(load_balancing: LbMethod) -> RuntimeUpstream {
    RuntimeUpstream::from_config(UpstreamConfig {
        servers: vec![
            addr("127.0.0.1:8081"),
            addr("127.0.0.1:8082"),
            addr("127.0.0.1:8083"),
        ],
        load_balancing,
    })
}

#[test]
fn print_round_robin_selection_result() {
    let upstream = upstream(LbMethod::RoundRobin);

    println!("round_robin selection result:");
    for i in 0..8 {
        let selected = upstream.select_server(None);
        println!("request #{i}: {selected:?}");
    }
}

#[test]
fn print_ip_hash_same_client_result() {
    let upstream = upstream(LbMethod::IpHash);
    let remote_addr = addr("192.168.1.10:53000");

    println!("ip_hash same client result:");
    for i in 0..8 {
        let selected = upstream.select_server(Some(remote_addr));
        println!("request #{i}, remote={remote_addr}: {selected:?}");
    }
}

#[test]
fn print_ip_hash_different_clients_result() {
    let upstream = upstream(LbMethod::IpHash);
    let clients = [
        addr("192.168.1.10:53000"),
        addr("192.168.1.11:53001"),
        addr("192.168.1.12:53002"),
        addr("10.0.0.8:41000"),
    ];

    println!("ip_hash different clients result:");
    for client in clients {
        let selected = upstream.select_server(Some(client));
        println!("remote={client}: {selected:?}");
    }
}

#[test]
fn print_empty_upstream_result() {
    let upstream = RuntimeUpstream::from_config(UpstreamConfig {
        servers: vec![],
        load_balancing: LbMethod::RoundRobin,
    });

    println!("empty upstream result: {:?}", upstream.select_server(None));
}
