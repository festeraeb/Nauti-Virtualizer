//! Seeded gate for Nexus Resiliency §9 T3b (follow-up to T3): the control RPC client (`AgentClient`) falls back from
//! the direct path to the relay, like the bulk path does. Since T3 a public IP counts as "direct", so `prefer_direct`
//! drops the relay for an agent that advertises one; without a fallback such an agent is unreachable. Written by the
//! controller before the forge task; the forge must not edit this file.
use nauti_fabric::rpc::{AgentClient, CONTROL_DIRECT_TIMEOUT};

use iroh::{EndpointAddr, SecretKey, TransportAddr};
use std::time::{Duration, Instant};

/// One private (so "direct") IP where nothing answers, and no relay: the direct attempt runs out its timeout, and the
/// relay attempt then fails at once because there is no relay. That error can only come from the relay branch.
fn unreachable_direct_no_relay() -> EndpointAddr {
    let id = SecretKey::from_bytes(&[44u8; 32]).public();
    let ip: std::net::SocketAddr = "10.255.255.1:9".parse().unwrap();
    EndpointAddr { id, addrs: [TransportAddr::Ip(ip)].into_iter().collect() }
}

#[test]
fn the_control_direct_timeout_is_three_seconds() {
    assert_eq!(CONTROL_DIRECT_TIMEOUT, Duration::from_secs(3));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_direct_control_connect_falls_through_to_the_relay() {
    let t0 = Instant::now();
    let err = match AgentClient::connect(unreachable_direct_no_relay()).await {
        Ok(_) => panic!("nothing answers and there is no relay, the connect cannot succeed"),
        Err(e) => e,
    };
    let took = t0.elapsed();
    let msg = err.to_string();
    assert!(msg.contains("no relay"), "want the relay branch's error, got: {msg}");
    assert!(took >= CONTROL_DIRECT_TIMEOUT - Duration::from_millis(100), "returned after {took:?}: no bounded direct attempt");
    assert!(took < Duration::from_secs(25), "returned after {took:?}: the direct attempt was not bounded");
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_with_identity_takes_the_same_path() {
    let key = SecretKey::from_bytes(&[45u8; 32]);
    let err = match AgentClient::connect_with_identity(unreachable_direct_no_relay(), Some(key)).await {
        Ok(_) => panic!("cannot succeed"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("no relay"), "got: {err}");
}
