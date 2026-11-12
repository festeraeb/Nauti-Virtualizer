//! The direct attempt is direct only. Through the N0 preset, iroh's address
//! lookup learned the server's home relay, so a "direct" connect could end
//! on a relay without the direct -> relay fallback ever running (the forge
//! controller's finding: 1.45 s, no fallback logged). With NAUTI_RELAY_URL
//! set, a direct connect to an address whose only direct IP never answers
//! must fail, and Auto must reach the server through our relay after the
//! direct timeout.
//!
//! One test in this binary: it sets process environment variables.

use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iroh::{EndpointAddr, TransportAddr};
use iroh_relay::server::{CertConfig, RelayConfig as RelayServerConfig, Server, ServerConfig, TlsConfig};
use nauti_fabric::data_quic::{
    DIRECT_CONNECT_TIMEOUT, DataServer, QuicChunkSource, QuicDataClient, TransportPreference, serve_quic,
};
use nauti_fabric::rpc::RpcTransportError;
use rustls_pki_types::pem::PemObject;

#[tokio::test(flavor = "multi_thread")]
async fn the_direct_attempt_never_ends_on_a_relay() {
    // Our relay, with a certificate this test trusts through NAUTI_RELAY_CA.
    let (certs, server_config) = iroh_relay::server::testing::self_signed_tls_certs_and_config();
    let tls = TlsConfig::new((std::net::Ipv4Addr::LOCALHOST, 0), CertConfig::Manual { server_config });
    let mut relay = RelayServerConfig::new((std::net::Ipv4Addr::LOCALHOST, 0));
    relay.tls = Some(tls);
    let mut config = ServerConfig::default();
    config.relay = Some(relay);
    let relay_server = Server::spawn(config).await.expect("relay");
    let url = format!("https://{}", relay_server.https_addr().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let ca = dir.path().join("relay-ca.pem");
    {
        let mut f = std::fs::File::create(&ca).unwrap();
        for c in &certs {
            let b64 = base64_encode(c.as_ref());
            writeln!(f, "-----BEGIN CERTIFICATE-----").unwrap();
            for chunk in b64.as_bytes().chunks(64) {
                writeln!(f, "{}", std::str::from_utf8(chunk).unwrap()).unwrap();
            }
            writeln!(f, "-----END CERTIFICATE-----").unwrap();
        }
    }
    assert_eq!(rustls_pki_types::CertificateDer::pem_file_iter(&ca).unwrap().count(), certs.len());
    // SAFETY: the only test in this binary; set before any endpoint exists.
    unsafe {
        std::env::set_var("NAUTI_RELAY_URL", &url);
        std::env::set_var("NAUTI_RELAY_CA", &ca);
    }

    let files = tempfile::tempdir().unwrap();
    std::fs::write(files.path().join("m.gguf"), vec![7u8; 4096]).unwrap();
    let (_router, served) = tokio::time::timeout(
        Duration::from_secs(20),
        serve_quic(Arc::new(DataServer::new(files.path(), None)), None),
    )
    .await
    .expect("the server reaches our relay (trusted through NAUTI_RELAY_CA)")
    .unwrap();
    let relay_url = served
        .addrs
        .iter()
        .find_map(|a| match a {
            TransportAddr::Relay(u) => Some(u.clone()),
            _ => None,
        })
        .expect("the server advertises our relay");
    assert!(relay_url.to_string().starts_with(&url));

    // A direct IP that never answers, plus our relay.
    let dead: std::net::SocketAddr = "10.255.255.1:9".parse().unwrap();
    let addr = EndpointAddr {
        id: served.id,
        addrs: [TransportAddr::Ip(dead), TransportAddr::Relay(relay_url)].into_iter().collect(),
    };

    let t0 = Instant::now();
    let direct = QuicDataClient::connect(addr.clone(), false).await;
    assert!(
        matches!(direct, Err(RpcTransportError::Timeout(_))),
        "the direct attempt must not succeed through a relay (took {:?}, relayed = {:?})",
        t0.elapsed(),
        direct.as_ref().ok().map(|c| c.is_relayed())
    );

    let t0 = Instant::now();
    let mut auto = QuicDataClient::connect_with(addr.clone(), TransportPreference::Auto).await.expect("auto");
    assert!(t0.elapsed() >= DIRECT_CONNECT_TIMEOUT - Duration::from_millis(100), "{:?}", t0.elapsed());
    assert!(auto.is_relayed(), "auto ends on the relay: {}", auto.path());
    assert_eq!(auto.stream.stat("m.gguf").await.unwrap(), 4096);

    // The bulk entry point the CLI uses, the same way.
    let rt_addr = addr.clone();
    let mut src = tokio::task::spawn_blocking(move || QuicChunkSource::connect_auto(rt_addr, "m.gguf"))
        .await
        .unwrap()
        .expect("connect_auto");
    let size = tokio::task::spawn_blocking(move || src.stat()).await.unwrap().unwrap();
    assert_eq!(size, 4096);
}

fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}
