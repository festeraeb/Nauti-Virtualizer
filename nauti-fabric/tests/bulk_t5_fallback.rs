//! Seeded gate for Nexus Resiliency §9 T5: `connect_direct_then_relay` tries the direct path first and, when that
//! fails, the relay. Written by the controller before the forge task; the forge must not edit this file.
use nauti_fabric::data_quic::{DIRECT_CONNECT_TIMEOUT, connect_direct_then_relay};
use nauti_fabric::rpc::RpcTransportError;

use iroh::{EndpointAddr, SecretKey, TransportAddr};
use std::time::{Duration, Instant};

/// One private (so "direct") IP where nothing answers, and no relay: the direct attempt times out, and the relay
/// attempt then fails at once with "the server advertises no relay". That error can only come from the relay call.
fn unreachable_direct_no_relay() -> EndpointAddr {
    let id = SecretKey::from_bytes(&[42u8; 32]).public();
    let ip: std::net::SocketAddr = "10.255.255.1:9".parse().unwrap();
    EndpointAddr { id, addrs: [TransportAddr::Ip(ip)].into_iter().collect() }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_direct_attempt_falls_through_to_the_relay() {
    let t0 = Instant::now();
    let err = match connect_direct_then_relay(unreachable_direct_no_relay()).await {
        Ok(_) => panic!("nothing answers and there is no relay, the connect cannot succeed"),
        Err(e) => e,
    };
    let took = t0.elapsed();
    // the relay call's error, not the direct call's Timeout: the Err branch reached the relay call
    assert!(matches!(&err, RpcTransportError::Iroh(m) if m.contains("no relay")), "want the relay error, got {err:?}");
    // and the direct attempt really ran first, for its full timeout
    assert!(took >= DIRECT_CONNECT_TIMEOUT - Duration::from_millis(100), "returned after {took:?}: no direct attempt");
    assert!(took < Duration::from_secs(10), "returned after {took:?}");
}
