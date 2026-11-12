//! The fabric relay end to end: two endpoints built by `fabric_endpoint_with`
//! with a local iroh-relay as the custom relay reach each other through that
//! relay, and no n0 relay appears anywhere.

use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::tls::CaTlsConfig;
use iroh::{RelayMode, RelayUrl, TransportAddr, Watcher};
use nauti_fabric::data_quic::relay_only;
use nauti_fabric::fabric_relay::fabric_endpoint_with;

const ALPN: &[u8] = b"nauti/test-relay-echo/0";

#[derive(Debug, Clone)]
struct Echo;

impl ProtocolHandler for Echo {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let (mut send, mut recv) = connection.accept_bi().await?;
        let data = recv.read_to_end(1024).await.map_err(AcceptError::from_err)?;
        send.write_all(&data).await.map_err(AcceptError::from_err)?;
        send.finish()?;
        connection.closed().await;
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn endpoints_use_the_custom_relay_and_never_n0() {
    let (_map, relay_url, _server) = iroh::test_utils::run_relay_server().await.expect("relay server");
    let url_text = relay_url.to_string();
    let n0: Vec<RelayUrl> = RelayMode::Default.relay_map().urls();

    // The test relay has a self-signed certificate.
    let server_ep = fabric_endpoint_with(Some(&url_text), None)
        .unwrap()
        .ca_tls_config(CaTlsConfig::insecure_skip_verify())
        .bind()
        .await
        .unwrap();
    server_ep.online().await;
    let router = Router::builder(server_ep.clone()).accept(ALPN, Echo).spawn();

    // The server's advertised relay is ours alone.
    let addr = server_ep.addr();
    let relays: Vec<&RelayUrl> = addr
        .addrs
        .iter()
        .filter_map(|a| match a {
            TransportAddr::Relay(u) => Some(u),
            _ => None,
        })
        .collect();
    assert_eq!(relays, vec![&relay_url]);
    let home: Vec<RelayUrl> = server_ep.home_relay_status().get().iter().map(|s| s.url().clone()).collect();
    assert!(home.iter().all(|u| *u == relay_url), "home relays {home:?}");
    assert!(home.iter().all(|u| !n0.contains(u)));

    // A client that may use the relay only reaches the server through it.
    let client_ep = fabric_endpoint_with(Some(&url_text), None)
        .unwrap()
        .ca_tls_config(CaTlsConfig::insecure_skip_verify())
        .bind()
        .await
        .unwrap();
    let connection = client_ep.connect(relay_only(addr), ALPN).await.expect("connect via relay");
    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    send.write_all(b"over our relay").await.unwrap();
    send.finish().unwrap();
    let echoed = recv.read_to_end(1024).await.unwrap();
    assert_eq!(echoed, b"over our relay");

    let paths = connection.paths();
    let relay_paths: Vec<String> = paths
        .iter()
        .filter(|p| p.is_relay())
        .map(|p| format!("{:?}", p.remote_addr()))
        .collect();
    assert!(!relay_paths.is_empty(), "no relay path");
    for p in &relay_paths {
        assert!(p.contains(relay_url.as_str().trim_end_matches('/')), "relay path {p} is not our relay");
        for u in &n0 {
            assert!(!p.contains(u.as_str().trim_end_matches('/')), "relay path {p} lists n0 relay {u}");
        }
    }
    connection.close(0u32.into(), b"done");
    router.shutdown().await.unwrap();
}
