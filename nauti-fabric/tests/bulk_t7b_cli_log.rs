//! Seeded gate for Nexus Resiliency §9 T7b (follow-up to T7): `nauti data-bench` installs the tracing subscriber, so
//! the bulk path's log lines (the fallback's warn, the chosen-path info line from T7) reach the terminal. Written by
//! the controller before the forge task; the forge must not edit this file.
use iroh::{EndpointAddr, SecretKey, TransportAddr};
use std::process::Command;

/// One private (so "direct") IP where nothing answers, and no relay: quic-auto's direct attempt times out and the
/// fallback logs `warn!(.., "data quic: direct connect failed; trying the relay")` before the relay attempt fails.
fn unreachable_direct_no_relay() -> EndpointAddr {
    let id = SecretKey::from_bytes(&[46u8; 32]).public();
    let ip: std::net::SocketAddr = "10.255.255.1:9".parse().unwrap();
    EndpointAddr { id, addrs: [TransportAddr::Ip(ip)].into_iter().collect() }
}

#[test]
fn data_bench_prints_the_bulk_path_log_lines() {
    let addr = serde_json::to_string(&unreachable_direct_no_relay()).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_nauti"))
        .args(["data-bench", "--path", "quic-auto", "--file", "m.gguf", "--addr", &addr, "--count", "1"])
        .env_remove("RUST_LOG")
        .output()
        .expect("run nauti");
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(!out.status.success(), "cannot succeed against an unreachable server:\n{text}");
    assert!(text.contains("trying the relay"), "the fallback's warn line should reach the terminal:\n{text}");
}
