//! T8a: the transport preference chooses between the direct path and the
//! relay, with one check per variant. The server address has one private
//! ("direct") IP where nothing answers and no relay, so each variant's
//! choice shows in what it returns and when.

use nauti_fabric::data_quic::{
    COMPLETION_FIRST_DIRECT_BUDGET, DIRECT_CONNECT_TIMEOUT, QuicDataClient, TransportPreference,
};
use nauti_fabric::rpc::RpcTransportError;

use iroh::{EndpointAddr, SecretKey, TransportAddr};
use std::time::{Duration, Instant};

fn unreachable_direct_no_relay() -> EndpointAddr {
    let id = SecretKey::from_bytes(&[43u8; 32]).public();
    let ip: std::net::SocketAddr = "10.255.255.1:9".parse().unwrap();
    EndpointAddr { id, addrs: [TransportAddr::Ip(ip)].into_iter().collect() }
}

fn relay_refusal(err: &RpcTransportError) -> bool {
    matches!(err, RpcTransportError::Iroh(m) if m.contains("advertises no relay"))
}

#[test]
fn transport_preference_tokens() {
    for (token, pref) in [
        ("auto", TransportPreference::Auto),
        ("completion-first", TransportPreference::CompletionFirst),
        ("direct-only", TransportPreference::DirectOnly),
    ] {
        assert_eq!(token.parse::<TransportPreference>().unwrap(), pref);
        assert_eq!(pref.to_string(), token);
    }
    assert!("urgent".parse::<TransportPreference>().is_err());
    assert_eq!(TransportPreference::default(), TransportPreference::Auto);
    assert!(COMPLETION_FIRST_DIRECT_BUDGET < DIRECT_CONNECT_TIMEOUT);
}

#[tokio::test(flavor = "multi_thread")]
async fn direct_only_never_tries_the_relay() {
    let t0 = Instant::now();
    let err = QuicDataClient::connect_with(unreachable_direct_no_relay(), TransportPreference::DirectOnly)
        .await
        .err()
        .expect("nothing answers");
    assert!(matches!(err, RpcTransportError::Timeout(d) if d == DIRECT_CONNECT_TIMEOUT), "{err:?}");
    assert!(t0.elapsed() >= Duration::from_millis(2900));
}

#[tokio::test(flavor = "multi_thread")]
async fn auto_waits_out_the_direct_timeout_then_tries_the_relay() {
    let t0 = Instant::now();
    let err = QuicDataClient::connect_with(unreachable_direct_no_relay(), TransportPreference::Auto)
        .await
        .err()
        .expect("nothing answers");
    let took = t0.elapsed();
    assert!(relay_refusal(&err), "the relay attempt is the last error: {err:?}");
    assert!(took >= Duration::from_millis(2900), "relay tried after {took:?}, before the direct timeout");
    assert!(took < Duration::from_secs(10));
}

#[tokio::test(flavor = "multi_thread")]
async fn completion_first_goes_to_the_relay_when_direct_is_not_immediate() {
    let t0 = Instant::now();
    let err = QuicDataClient::connect_with(unreachable_direct_no_relay(), TransportPreference::CompletionFirst)
        .await
        .err()
        .expect("nothing answers");
    let took = t0.elapsed();
    assert!(relay_refusal(&err), "the relay attempt is the last error: {err:?}");
    assert!(took >= COMPLETION_FIRST_DIRECT_BUDGET - Duration::from_millis(50), "{took:?}");
    assert!(took < Duration::from_millis(2500), "waited {took:?}: completion first must not wait for the 3 s direct timeout");
}
