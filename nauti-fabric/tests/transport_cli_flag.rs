//! `nauti data-bench --transport` picks the TransportPreference for the
//! quic-auto path.
use iroh::{EndpointAddr, SecretKey, TransportAddr};
use std::process::Command;
use std::time::{Duration, Instant};

fn unreachable_direct_no_relay() -> String {
    let id = SecretKey::from_bytes(&[47u8; 32]).public();
    let ip: std::net::SocketAddr = "10.255.255.1:9".parse().unwrap();
    serde_json::to_string(&EndpointAddr { id, addrs: [TransportAddr::Ip(ip)].into_iter().collect() }).unwrap()
}

fn run(transport: &str) -> (bool, String, Duration) {
    let t0 = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_nauti"))
        .args(["data-bench", "--path", "quic-auto", "--file", "m.gguf", "--addr", &unreachable_direct_no_relay()])
        .args(["--count", "1", "--transport", transport])
        .env_remove("NAUTI_RELAY_URL")
        .output()
        .expect("run nauti");
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text, t0.elapsed())
}

#[test]
fn direct_only_never_tries_the_relay() {
    let (ok, text, _) = run("direct-only");
    assert!(!ok);
    assert!(text.contains("Timeout(3s)") || text.contains("timed out"), "{text}");
    assert!(!text.contains("trying the relay"), "{text}");
}

#[test]
fn completion_first_goes_to_the_relay_quickly() {
    let (ok, text, took) = run("completion-first");
    assert!(!ok);
    assert!(text.contains("completion first"), "{text}");
    assert!(took < Duration::from_millis(2900) + Duration::from_secs(2), "{took:?}");
}

#[test]
fn an_unknown_transport_is_refused_with_the_choices() {
    let (ok, text, _) = run("urgent");
    assert!(!ok);
    assert!(text.contains("auto | completion-first | direct-only"), "{text}");
}
