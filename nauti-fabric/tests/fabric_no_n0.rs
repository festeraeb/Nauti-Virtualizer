//! With a custom relay (NAUTI_RELAY_URL), a fabric endpoint must not reach
//! any n0 service: no n0 relay, no n0 address lookup (pkarr publish/resolve
//! or DNS). Every n0 service is a hostname under iroh.link, so the test
//! points the endpoints' DNS resolver at a local fake DNS server that
//! records every name asked and answers NXDOMAIN. Any iroh.link name in the
//! record fails the test.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::dns::DnsResolver;
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::tls::CaTlsConfig;
use iroh::TransportAddr;
use nauti_fabric::data_quic::relay_only;
use nauti_fabric::fabric_relay::{direct_endpoint, fabric_endpoint_with};

const ALPN: &[u8] = b"nauti/test-no-n0/0";

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

/// QNAME of a DNS query, dotted, lower case.
fn qname(packet: &[u8]) -> Option<String> {
    let mut i = 12;
    let mut labels = Vec::new();
    while i < packet.len() {
        let len = packet[i] as usize;
        if len == 0 {
            return Some(labels.join(".").to_lowercase());
        }
        let label = packet.get(i + 1..i + 1 + len)?;
        labels.push(String::from_utf8_lossy(label).into_owned());
        i += 1 + len;
    }
    None
}

/// A DNS server on 127.0.0.1 that records names and answers NXDOMAIN.
async fn fake_dns() -> (std::net::SocketAddr, Arc<Mutex<Vec<String>>>) {
    let sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = sock.local_addr().unwrap();
    let names = Arc::new(Mutex::new(Vec::new()));
    let record = names.clone();
    tokio::spawn(async move {
        let mut buf = [0u8; 1500];
        loop {
            let Ok((n, from)) = sock.recv_from(&mut buf).await else { break };
            if n < 12 {
                continue;
            }
            if let Some(name) = qname(&buf[..n]) {
                record.lock().unwrap().push(name);
            }
            let mut reply = buf[..n].to_vec();
            reply[2] = 0x81 | (buf[2] & 0x01); // QR, opcode 0, RD copied
            reply[3] = 0x83; // RA, NXDOMAIN
            let _ = sock.send_to(&reply, from).await;
        }
    });
    (addr, names)
}

#[tokio::test(flavor = "multi_thread")]
async fn with_a_custom_relay_no_n0_service_is_contacted() {
    let (dns, names) = fake_dns().await;
    let (_map, relay_url, _relay) = iroh::test_utils::run_relay_server().await.expect("relay");
    let url = relay_url.to_string();

    let server = fabric_endpoint_with(Some(&url), None)
        .unwrap()
        .dns_resolver(DnsResolver::with_nameserver(dns))
        .ca_tls_config(CaTlsConfig::insecure_skip_verify())
        .bind()
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), server.online()).await.expect("home relay");
    let router = Router::builder(server.clone()).accept(ALPN, Echo).spawn();
    let addr = server.addr();

    // The direct attempt: an endpoint that has no relay and no lookup.
    let direct = direct_endpoint(None)
        .unwrap()
        .dns_resolver(DnsResolver::with_nameserver(dns))
        .bind()
        .await
        .unwrap();
    let direct_only = iroh::EndpointAddr {
        id: addr.id,
        addrs: addr.addrs.iter().filter(|a| matches!(a, TransportAddr::Ip(_))).cloned().collect(),
    };
    let c = direct.connect(direct_only, ALPN).await.expect("direct on loopback");
    assert!(c.paths().iter().all(|p| !p.is_relay()), "a direct connection has no relay path");
    c.close(0u32.into(), b"done");

    // The relay attempt.
    let relayed = fabric_endpoint_with(Some(&url), None)
        .unwrap()
        .dns_resolver(DnsResolver::with_nameserver(dns))
        .ca_tls_config(CaTlsConfig::insecure_skip_verify())
        .bind()
        .await
        .unwrap();
    let c = relayed.connect(relay_only(addr), ALPN).await.expect("via our relay");
    let (mut send, mut recv) = c.open_bi().await.unwrap();
    send.write_all(b"x").await.unwrap();
    send.finish().unwrap();
    assert_eq!(recv.read_to_end(16).await.unwrap(), b"x");

    // Address publishing runs in the background: give it time to try.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let asked = names.lock().unwrap().clone();
    let n0: Vec<&String> = asked.iter().filter(|n| n.contains("iroh.link")).collect();
    assert!(n0.is_empty(), "n0 names were resolved: {n0:?} (all names: {asked:?})");
    router.shutdown().await.unwrap();
}
