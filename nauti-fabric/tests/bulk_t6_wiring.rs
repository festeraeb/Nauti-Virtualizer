//! Seeded gate for Nexus Resiliency §9 T6: the bulk entry (`QuicChunkSource::connect_auto`, and `nauti data-bench
//! --path quic-auto`) goes through the direct->relay fallback, while the bench's `quic` and `quic-relay` paths stay
//! single-path. Written by the controller before the forge task; the forge must not edit this file.
use nauti_fabric::data_quic::QuicChunkSource;

use iroh::{EndpointAddr, SecretKey, TransportAddr};
use std::process::Command;
use std::time::{Duration, Instant};

/// One private (so "direct") IP where nothing answers, and no relay. Through the fallback the direct attempt
/// times out and the relay attempt fails with "the server advertises no relay"; that text proves the relay call ran.
fn unreachable_direct_no_relay() -> EndpointAddr {
    let id = SecretKey::from_bytes(&[43u8; 32]).public();
    let ip: std::net::SocketAddr = "10.255.255.1:9".parse().unwrap();
    EndpointAddr { id, addrs: [TransportAddr::Ip(ip)].into_iter().collect() }
}

/// Runs `nauti data-bench --path <path>` against the unreachable address; returns stdout+stderr (it must fail).
fn bench(path: &str) -> String {
    let addr = serde_json::to_string(&unreachable_direct_no_relay()).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_nauti"))
        .args(["data-bench", "--path", path, "--file", "m.gguf", "--addr", &addr, "--count", "1"])
        .output()
        .expect("run nauti");
    assert!(!out.status.success(), "--path {path} cannot succeed against an unreachable server");
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

fn says_timeout(s: &str) -> bool {
    s.contains("timed out") || s.contains("Timeout")
}

#[test]
fn connect_auto_goes_through_the_fallback() {
    let t0 = Instant::now();
    let err = match QuicChunkSource::connect_auto(unreachable_direct_no_relay(), "m.gguf") {
        Ok(_) => panic!("nothing answers and there is no relay, connect_auto cannot succeed"),
        Err(e) => e,
    };
    let took = t0.elapsed();
    assert!(err.to_string().contains("no relay"), "want the relay attempt's error, got {err}");
    assert!(took >= Duration::from_millis(2900), "returned after {took:?}: the direct attempt did not run first");
}

#[test]
fn data_bench_quic_auto_uses_the_fallback() {
    let out = bench("quic-auto");
    assert!(!out.contains("nfs-mmap, quic, quic-relay, tcp or roce"), "quic-auto is not a known --path yet:\n{out}");
    assert!(out.contains("no relay"), "quic-auto should end at the relay attempt:\n{out}");
}

#[test]
fn data_bench_quic_stays_direct_only() {
    let out = bench("quic");
    assert!(says_timeout(&out), "quic should end in the direct timeout:\n{out}");
    assert!(!out.contains("no relay"), "quic must not fall back to the relay:\n{out}");
}

#[test]
fn data_bench_quic_relay_stays_relay_only() {
    let out = bench("quic-relay");
    assert!(out.contains("no relay"), "quic-relay should fail on the missing relay:\n{out}");
    assert!(!says_timeout(&out), "quic-relay must not make a direct attempt:\n{out}");
}
