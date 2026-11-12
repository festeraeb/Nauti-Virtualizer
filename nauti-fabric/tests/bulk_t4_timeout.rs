//! Seeded gate for Nexus Resiliency §9 T4: the direct QUIC data connect gets a 3 s timeout and a typed error.
//! Written by the controller before the forge task; the forge must not edit this file.
use nauti_fabric::data_quic::{DIRECT_CONNECT_TIMEOUT, QuicDataClient};
use nauti_fabric::rpc::RpcTransportError;

use iroh::{EndpointAddr, SecretKey, TransportAddr};
use std::time::{Duration, Instant};

/// A server address with one private (so "direct") IP where nothing answers, and no relay.
fn unreachable_direct_no_relay() -> EndpointAddr {
    let id = SecretKey::from_bytes(&[41u8; 32]).public();
    let ip: std::net::SocketAddr = "10.255.255.1:9".parse().unwrap();
    EndpointAddr { id, addrs: [TransportAddr::Ip(ip)].into_iter().collect() }
}

#[test]
fn the_direct_timeout_is_three_seconds() {
    assert_eq!(DIRECT_CONNECT_TIMEOUT, Duration::from_secs(3));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_direct_connect_that_nobody_answers_ends_in_a_typed_timeout() {
    let t0 = Instant::now();
    let err = match QuicDataClient::connect(unreachable_direct_no_relay(), false).await {
        Ok(_) => panic!("nothing listens at 10.255.255.1:9, the connect cannot succeed"),
        Err(e) => e,
    };
    let took = t0.elapsed();
    assert!(
        matches!(err, RpcTransportError::Timeout(d) if d == DIRECT_CONNECT_TIMEOUT),
        "want RpcTransportError::Timeout(DIRECT_CONNECT_TIMEOUT), got {err:?}"
    );
    assert!(took >= Duration::from_millis(2900), "returned after {took:?}, before the 3 s timeout");
    assert!(took < Duration::from_secs(10), "returned after {took:?}, the timeout did not bound the connect");
    assert!(err.to_string().contains("timed out"), "Display should say 'timed out': {err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_relay_branch_is_unchanged() {
    let t0 = Instant::now();
    let err = match QuicDataClient::connect(unreachable_direct_no_relay(), true).await {
        Ok(_) => panic!("the address has no relay, a relay connect cannot succeed"),
        Err(e) => e,
    };
    assert!(matches!(&err, RpcTransportError::Iroh(m) if m.contains("no relay")), "got {err:?}");
    assert!(t0.elapsed() < Duration::from_secs(2), "relay=true must not make a direct attempt first");
}
