//! The data protocol over the fabric's own transports, and the measurement that puts them beside the direct paths.
//!
//! A mesh (Nexus, or the fabric's Iroh control link) can carry bulk bytes: a request names a file and a range, the
//! bytes come back on the same stream.  TRANSPORT_ANALYSIS.md says not to force bulk data through that link; this
//! module makes the comparison measurable.  The same frames go over Iroh/QUIC ([`DATA_ALPN`]: direct, or relay
//! only) and over plain TCP, and `data::bench` reads the same scattered chunks through each, through the NFS
//! mapping ([`crate::data::MappedFile`]) and, with the `rdma` feature, by RDMA READ.  RdmaHello is the control
//! message of that last path: the queue pair numbers and the registered window travel here, the bytes do not.
//!
//! Framing is `rpc`'s: a u32 little-endian length and JSON; a Bytes response is followed by its raw payload.
use crate::data::{ChunkSource, MappedFile};
use crate::rpc::{RpcTransportError, prefer_direct, read_frame, write_frame};

use iroh::endpoint::{Connection, presets};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr, SecretKey, TransportAddr};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// ALPN of the data protocol.
pub const DATA_ALPN: &[u8] = b"nauti-fabric/data/1";

/// How long a direct (non-relay) data connect may take before it is given up.
pub const DIRECT_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// A queue pair's numbers, as the control link carries them (gid as 32 hex digits).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QpInfo {
    pub qpn: u32,
    pub psn: u32,
    pub gid: String,
    pub lid: u16,
    pub mtu: i32,
}

impl QpInfo {
    pub fn gid_bytes(&self) -> io::Result<[u8; 16]> {
        let mut g = [0u8; 16];
        if self.gid.len() != 32 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "a GID is 32 hex digits"));
        }
        for (i, b) in g.iter_mut().enumerate() {
            *b = u8::from_str_radix(&self.gid[2 * i..2 * i + 2], 16)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "a GID is hex"))?;
        }
        Ok(g)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum DataRequest {
    /// The file's size.
    Stat { file: String },
    /// `len` bytes at `offset`: answered Bytes, then the bytes.
    Get { file: String, offset: u64, len: u32 },
    /// The client's queue pair and the window it will RDMA READ; answered with the server's queue pair and the
    /// window's registered address and key.
    RdmaHello { file: String, offset: u64, len: u64, qp: QpInfo },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum DataResponse {
    Stat { bytes: u64 },
    Bytes { len: u32 },
    Error { reason: String },
    RdmaHello { qp: QpInfo, addr: u64, rkey: u32, len: u64 },
}

/// Files under a root, mapped once each, served by range.
pub struct DataServer {
    root: PathBuf,
    files: Mutex<HashMap<PathBuf, Arc<MappedFile>>>,
    /// device, port, gid index of the RoCE port RdmaHello registers windows on
    rdma: Option<(String, i32, i32)>,
}

impl DataServer {
    pub fn new(root: impl Into<PathBuf>, rdma: Option<(String, i32, i32)>) -> Self {
        Self { root: root.into(), files: Mutex::new(HashMap::new()), rdma }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn file(&self, name: &str) -> io::Result<Arc<MappedFile>> {
        let rel = Path::new(name);
        if rel.is_absolute() || rel.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "a file is named under the root, without .."));
        }
        let path = self.root.join(rel);
        let mut files = self.files.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(m) = files.get(&path) {
            return Ok(m.clone());
        }
        let m = Arc::new(MappedFile::open(&path)?);
        files.insert(path, m.clone());
        Ok(m)
    }

    /// One connection's requests, whatever carries them, until it closes.
    pub async fn handle<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
        &self,
        recv: &mut R,
        send: &mut W,
    ) -> Result<(), RpcTransportError> {
        #[cfg(feature = "rdma")]
        let mut windows: Vec<(crate::rdma::RdmaPort, crate::rdma::MemoryRegion, Arc<MappedFile>)> = Vec::new();
        let mut buf: Vec<u8> = Vec::new();
        loop {
            let request: DataRequest = match read_frame(recv).await {
                Ok(r) => r,
                Err(RpcTransportError::ConnectionClosed) => return Ok(()),
                Err(e) => return Err(e),
            };
            match request {
                DataRequest::Stat { file } => {
                    let reply = match self.file(&file) {
                        Ok(m) => DataResponse::Stat { bytes: m.len() },
                        Err(e) => DataResponse::Error { reason: format!("{file}: {e}") },
                    };
                    write_frame(send, &reply).await?;
                }
                DataRequest::Get { file, offset, len } => {
                    let outcome = self.file(&file).and_then(|m| {
                        buf.resize(len as usize, 0);
                        m.read_at(offset, &mut buf)
                    });
                    match outcome {
                        Ok(()) => {
                            write_frame(send, &DataResponse::Bytes { len }).await?;
                            send.write_all(&buf).await?;
                        }
                        Err(e) => write_frame(send, &DataResponse::Error { reason: format!("{file}: {e}") }).await?,
                    }
                }
                DataRequest::RdmaHello { file, offset, len, qp } => {
                    #[cfg(feature = "rdma")]
                    let reply = self.rdma_hello(&file, offset, len, &qp, &mut windows);
                    #[cfg(not(feature = "rdma"))]
                    let reply = self.rdma_hello(&file, offset, len, &qp);
                    write_frame(send, &reply).await?;
                }
            }
            send.flush().await?;
        }
    }

    #[cfg(feature = "rdma")]
    fn rdma_hello(
        &self,
        file: &str,
        offset: u64,
        len: u64,
        qp: &QpInfo,
        windows: &mut Vec<(crate::rdma::RdmaPort, crate::rdma::MemoryRegion, Arc<MappedFile>)>,
    ) -> DataResponse {
        let Some((dev, port, gid)) = &self.rdma else {
            return DataResponse::Error { reason: "this server has no RDMA port (start it with --rdma)".into() };
        };
        let mut attempt = || -> io::Result<DataResponse> {
            let m = self.file(file)?;
            if offset.checked_add(len).is_none_or(|end| end > m.len()) {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "the window runs past the file"));
            }
            let p = crate::rdma::RdmaPort::open(dev, *port, *gid)?;
            let region = p.register(m.addr_of(offset)?, len as usize, true)?;
            p.connect(qp)?;
            let reply = DataResponse::RdmaHello { qp: p.local_info(), addr: region.addr, rkey: region.rkey, len };
            windows.push((p, region, m));
            Ok(reply)
        };
        attempt().unwrap_or_else(|e| DataResponse::Error { reason: format!("rdma window of {file}: {e}") })
    }

    #[cfg(not(feature = "rdma"))]
    fn rdma_hello(&self, _file: &str, _offset: u64, _len: u64, _qp: &QpInfo) -> DataResponse {
        let _ = &self.rdma;
        DataResponse::Error { reason: "this nauti was built without the rdma feature".into() }
    }
}

/// The Iroh protocol handler over a shared [`DataServer`].
#[derive(Clone)]
pub struct DataService(pub Arc<DataServer>);

impl std::fmt::Debug for DataService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DataService").field("root", &self.0.root).finish()
    }
}

impl ProtocolHandler for DataService {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let (mut send, mut recv) = connection.accept_bi().await?;
        self.0.handle(&mut recv, &mut send).await.map_err(AcceptError::from_err)?;
        send.finish()?;
        connection.closed().await;
        Ok(())
    }
}

/// Serves the data protocol over Iroh/QUIC; the address to give clients comes back.
pub async fn serve_quic(
    server: Arc<DataServer>,
    secret_key: Option<SecretKey>,
) -> Result<(Router, EndpointAddr), RpcTransportError> {
    let mut builder = Endpoint::builder(presets::N0);
    if let Some(key) = secret_key {
        builder = builder.secret_key(key);
    }
    let endpoint = builder.bind().await.map_err(|e| RpcTransportError::Iroh(e.to_string()))?;
    endpoint.online().await;
    let addr = endpoint.addr();
    let router = Router::builder(endpoint).accept(DATA_ALPN, DataService(server)).spawn();
    Ok((router, addr))
}

/// Serves the same frames over plain TCP, one task a connection, until the task is dropped.
pub async fn serve_tcp(server: Arc<DataServer>, listen: std::net::SocketAddr) -> io::Result<()> {
    let listener = tokio::net::TcpListener::bind(listen).await?;
    loop {
        let (stream, peer) = listener.accept().await?;
        let _ = stream.set_nodelay(true);
        let server = server.clone();
        tokio::spawn(async move {
            let (mut r, mut w) = stream.into_split();
            if let Err(e) = server.handle(&mut r, &mut w).await {
                tracing::info!(%peer, error = %e, "data tcp: connection ended");
            }
        });
    }
}

/// Keeps only the relay transport addresses, so a connection must go through the relay.
pub fn relay_only(addr: EndpointAddr) -> EndpointAddr {
    let relays: std::collections::BTreeSet<TransportAddr> =
        addr.addrs.iter().filter(|a| matches!(a, TransportAddr::Relay(_))).cloned().collect();
    EndpointAddr { id: addr.id, addrs: relays }
}

/// A client of the data protocol over a transport's read and write halves.
pub struct DataStream<R, W> {
    recv: R,
    send: W,
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> DataStream<R, W> {
    pub fn new(recv: R, send: W) -> Self {
        Self { recv, send }
    }

    async fn call(&mut self, request: &DataRequest) -> Result<DataResponse, RpcTransportError> {
        write_frame(&mut self.send, request).await?;
        self.send.flush().await?;
        read_frame(&mut self.recv).await
    }

    pub async fn stat(&mut self, file: &str) -> Result<u64, RpcTransportError> {
        match self.call(&DataRequest::Stat { file: file.into() }).await? {
            DataResponse::Stat { bytes } => Ok(bytes),
            DataResponse::Error { reason } => Err(RpcTransportError::Io(io::Error::other(reason))),
            other => Err(RpcTransportError::Io(io::Error::other(format!("unexpected answer {other:?}")))),
        }
    }

    pub async fn get(&mut self, file: &str, offset: u64, dst: &mut [u8]) -> Result<(), RpcTransportError> {
        let len = u32::try_from(dst.len()).map_err(|_| RpcTransportError::FrameTooLarge(u32::MAX))?;
        match self.call(&DataRequest::Get { file: file.into(), offset, len }).await? {
            DataResponse::Bytes { len: got } if got == len => {
                self.recv.read_exact(dst).await?;
                Ok(())
            }
            DataResponse::Bytes { len: got } => {
                Err(RpcTransportError::Io(io::Error::other(format!("asked {len} bytes, offered {got}"))))
            }
            DataResponse::Error { reason } => Err(RpcTransportError::Io(io::Error::other(reason))),
            other => Err(RpcTransportError::Io(io::Error::other(format!("unexpected answer {other:?}")))),
        }
    }

    /// Hands the server this side's queue pair and the window to register; the server's numbers come back.
    pub async fn rdma_hello(
        &mut self,
        file: &str,
        offset: u64,
        len: u64,
        qp: QpInfo,
    ) -> Result<(QpInfo, u64, u32, u64), RpcTransportError> {
        match self.call(&DataRequest::RdmaHello { file: file.into(), offset, len, qp }).await? {
            DataResponse::RdmaHello { qp, addr, rkey, len } => Ok((qp, addr, rkey, len)),
            DataResponse::Error { reason } => Err(RpcTransportError::Io(io::Error::other(reason))),
            other => Err(RpcTransportError::Io(io::Error::other(format!("unexpected answer {other:?}")))),
        }
    }
}

/// A QUIC client: the endpoint, the connection and one stream.
pub struct QuicDataClient {
    _endpoint: Endpoint,
    connection: Connection,
    pub stream: DataStream<iroh::endpoint::RecvStream, iroh::endpoint::SendStream>,
}

impl QuicDataClient {
    /// Connects over the server's direct addresses, or, with `relay`, over its relay alone.
    pub async fn connect(addr: EndpointAddr, relay: bool) -> Result<Self, RpcTransportError> {
        // The no-relay refusal needs no endpoint: check before binding (bind
        // can block on relay DNS), so relay=true never makes a direct attempt.
        let addr = if relay {
            let a = relay_only(addr);
            if a.addrs.is_empty() {
                return Err(RpcTransportError::Iroh("the server advertises no relay".into()));
            }
            a
        } else {
            prefer_direct(addr)
        };
        let endpoint = Endpoint::bind(presets::N0).await.map_err(|e| RpcTransportError::Iroh(e.to_string()))?;
        if relay {
            endpoint.online().await;
        }
        let connection = if relay {
            endpoint.connect(addr, DATA_ALPN).await.map_err(|e| RpcTransportError::Iroh(e.to_string()))?
        } else {
            tokio::time::timeout(DIRECT_CONNECT_TIMEOUT, endpoint.connect(addr, DATA_ALPN))
                .await
                .map_err(|_| RpcTransportError::Timeout(DIRECT_CONNECT_TIMEOUT))?
                .map_err(|e| RpcTransportError::Iroh(e.to_string()))?
        };
        let (send, recv) = connection.open_bi().await.map_err(|e| RpcTransportError::Iroh(e.to_string()))?;
        Ok(Self { _endpoint: endpoint, connection, stream: DataStream::new(recv, send) })
    }

    /// The path the connection takes now: direct or relay, the remote address, its RTT.
    pub fn path(&self) -> String {
        let paths = self.connection.paths();
        let mut out = Vec::new();
        for p in paths.iter() {
            let kind = if p.is_relay() { "relay" } else { "direct" };
            let sel = if p.is_selected() { "*" } else { "" };
            out.push(format!("{sel}{kind} {:?} rtt {:.2} ms", p.remote_addr(), p.rtt().as_secs_f64() * 1e3));
        }
        if out.is_empty() { "no path".into() } else { out.join("; ") }
    }

    pub fn is_relayed(&self) -> bool {
        self.connection.paths().iter().any(|p| p.is_selected() && p.is_relay())
    }
}

/// The bulk path's observability line: the file, its byte length and the path the connection took.
pub fn bulk_path_line(file: &str, bytes: u64, relayed: bool, path: &str) -> String {
    format!("bulk {file}: {bytes} bytes over {} ({path})", if relayed { "relay" } else { "direct" })
}

/// The bulk path's connect: the direct path first, and when that fails (timeout or error), the relay.
pub async fn connect_direct_then_relay(addr: EndpointAddr) -> Result<QuicDataClient, RpcTransportError> {
    match QuicDataClient::connect(addr.clone(), false).await {
        Ok(client) => Ok(client),
        Err(e) => {
            tracing::warn!(error = %e, "data quic: direct connect failed; trying the relay");
            QuicDataClient::connect(addr, true).await
        }
    }
}

/// One file over QUIC, as a [`ChunkSource`] for the bench (a runtime of its own drives the async client).
pub struct QuicChunkSource {
    rt: tokio::runtime::Runtime,
    client: QuicDataClient,
    file: String,
    relay: bool,
}

impl QuicChunkSource {
    pub fn connect(addr: EndpointAddr, relay: bool, file: &str) -> io::Result<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
        let client = rt.block_on(QuicDataClient::connect(addr, relay)).map_err(io::Error::other)?;
        Ok(Self { rt, client, file: file.into(), relay })
    }

    /// The bulk path: the direct path first, the relay when that fails ([`connect_direct_then_relay`]).
    pub fn connect_auto(addr: EndpointAddr, file: &str) -> io::Result<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
        let mut client = rt.block_on(connect_direct_then_relay(addr)).map_err(io::Error::other)?;
        let bytes = rt.block_on(client.stream.stat(file)).map_err(io::Error::other)?;
        tracing::info!("{}", bulk_path_line(file, bytes, client.is_relayed(), &client.path()));
        let relay = client.is_relayed();
        Ok(Self { rt, client, file: file.into(), relay })
    }

    pub fn stat(&mut self) -> io::Result<u64> {
        self.rt.block_on(self.client.stream.stat(&self.file)).map_err(io::Error::other)
    }

    pub fn client(&mut self) -> (&tokio::runtime::Runtime, &mut QuicDataClient) {
        (&self.rt, &mut self.client)
    }
}

impl ChunkSource for QuicChunkSource {
    fn describe(&self) -> String {
        format!("quic{} ({})", if self.relay { "-relay" } else { "" }, self.client.path())
    }

    fn fetch(&mut self, offset: u64, dst: &mut [u8]) -> io::Result<()> {
        self.rt.block_on(self.client.stream.get(&self.file, offset, dst)).map_err(io::Error::other)
    }
}

/// One file over plain TCP.
pub struct TcpChunkSource {
    rt: tokio::runtime::Runtime,
    stream: DataStream<tokio::net::tcp::OwnedReadHalf, tokio::net::tcp::OwnedWriteHalf>,
    peer: std::net::SocketAddr,
    file: String,
}

impl TcpChunkSource {
    pub fn connect(peer: std::net::SocketAddr, file: &str) -> io::Result<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
        let s = rt.block_on(tokio::net::TcpStream::connect(peer))?;
        s.set_nodelay(true)?;
        let (r, w) = s.into_split();
        Ok(Self { rt, stream: DataStream::new(r, w), peer, file: file.into() })
    }

    pub fn stat(&mut self) -> io::Result<u64> {
        self.rt.block_on(self.stream.stat(&self.file)).map_err(io::Error::other)
    }
}

impl ChunkSource for TcpChunkSource {
    fn describe(&self) -> String {
        format!("tcp {}", self.peer)
    }

    fn fetch(&mut self, offset: u64, dst: &mut [u8]) -> io::Result<()> {
        self.rt.block_on(self.stream.get(&self.file, offset, dst)).map_err(io::Error::other)
    }
}

/// RDMA READs of a server's window, with QUIC as the control link.  The bench's own buffer is registered on the
/// first fetch and read into directly: no copy on either side.
#[cfg(feature = "rdma")]
pub struct RdmaChunkSource {
    _control: QuicChunkSource,
    port: crate::rdma::RdmaPort,
    local: Option<crate::rdma::MemoryRegion>,
    remote_addr: u64,
    rkey: u32,
    window_offset: u64,
    window_len: u64,
    label: String,
}

#[cfg(feature = "rdma")]
impl RdmaChunkSource {
    /// `spec` is this side's "device:port:gid_index".
    pub fn connect(
        addr: EndpointAddr,
        file: &str,
        spec: &str,
        window_offset: u64,
        window_len: u64,
    ) -> io::Result<Self> {
        let (dev, port_no, gid) = crate::rdma::parse_rdma_spec(spec)?;
        let port = crate::rdma::RdmaPort::open(&dev, port_no, gid)?;
        let mut control = QuicChunkSource::connect(addr, false, file)?;
        let (rt, client) = control.client();
        let (peer, remote_addr, rkey, len) = rt
            .block_on(client.stream.rdma_hello(file, window_offset, window_len, port.local_info()))
            .map_err(io::Error::other)?;
        port.connect(&peer)?;
        let label = format!("roce {dev} gid {gid} <- qp {} (mtu {})", peer.qpn, peer.mtu);
        Ok(Self { _control: control, port, local: None, remote_addr, rkey, window_offset, window_len: len, label })
    }
}

#[cfg(feature = "rdma")]
impl ChunkSource for RdmaChunkSource {
    fn describe(&self) -> String {
        self.label.clone()
    }

    fn fetch(&mut self, offset: u64, dst: &mut [u8]) -> io::Result<()> {
        if offset < self.window_offset || offset + dst.len() as u64 > self.window_offset + self.window_len {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "the chunk is outside the registered window"));
        }
        let same = self.local.as_ref().is_some_and(|r| r.addr == dst.as_ptr() as u64 && r.len >= dst.len());
        if !same {
            self.local = Some(self.port.register(dst.as_ptr(), dst.len(), false)?);
        }
        let local = self.local.as_ref().expect("registered above");
        self.port.read(local, 0, self.remote_addr + (offset - self.window_offset), self.rkey, dst.len(), 30_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn qp_info_round_trips_its_gid() {
        let q = QpInfo { qpn: 5, psn: 9, gid: "00000000000000000000ffff0a000a3d".into(), lid: 0, mtu: 5 };
        let g = q.gid_bytes().unwrap();
        assert_eq!(&g[12..], &[10, 0, 10, 61]);
        assert!(QpInfo { gid: "zz".into(), ..q }.gid_bytes().is_err());
    }

    #[test]
    fn requests_and_responses_are_json() {
        let r = DataRequest::Get { file: "a.gguf".into(), offset: 4096, len: 24576 };
        let back: DataRequest = serde_json::from_slice(&serde_json::to_vec(&r).unwrap()).unwrap();
        assert_eq!(back, r);
        let e = DataResponse::Error { reason: "no".into() };
        assert_eq!(serde_json::from_str::<DataResponse>(&serde_json::to_string(&e).unwrap()).unwrap(), e);
    }

    #[test]
    fn the_server_keeps_to_its_root() {
        let dir = tempfile::tempdir().unwrap();
        let s = DataServer::new(dir.path(), None);
        assert!(s.file("../etc/passwd").is_err());
        assert!(s.file("/etc/passwd").is_err());
        assert!(s.file("missing").is_err());
        std::fs::write(dir.path().join("f"), b"hello").unwrap();
        assert_eq!(s.file("f").unwrap().len(), 5);
        assert!(Arc::ptr_eq(&s.file("f").unwrap(), &s.file("f").unwrap()), "mapped once");
    }

    #[test]
    fn tcp_serves_stat_and_ranges_and_refuses_what_it_cannot() {
        let dir = tempfile::tempdir().unwrap();
        let bytes: Vec<u8> = (0..100_000u32).map(|i| (i % 253) as u8).collect();
        std::fs::File::create(dir.path().join("m.gguf")).unwrap().write_all(&bytes).unwrap();
        let server = Arc::new(DataServer::new(dir.path(), None));
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let listener = rt.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
        let peer = listener.local_addr().unwrap();
        let srv = server.clone();
        rt.spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let srv = srv.clone();
                tokio::spawn(async move {
                    let (mut r, mut w) = stream.into_split();
                    let _ = srv.handle(&mut r, &mut w).await;
                });
            }
        });
        let mut c = TcpChunkSource::connect(peer, "m.gguf").unwrap();
        assert_eq!(c.stat().unwrap(), 100_000);
        let mut dst = vec![0u8; 1000];
        c.fetch(50_000, &mut dst).unwrap();
        assert_eq!(&dst[..], &bytes[50_000..51_000]);
        assert!(c.fetch(99_500, &mut dst).is_err(), "past the end is an error, and the stream goes on");
        c.fetch(0, &mut dst).unwrap();
        assert_eq!(&dst[..], &bytes[..1000]);
        let mut missing = TcpChunkSource::connect(peer, "nope").unwrap();
        assert!(missing.stat().is_err());
        let offsets = crate::data::scattered_offsets(0, 100_000, 1000, 30, 3);
        let r = crate::data::bench(&mut c, &offsets, 1000, false).unwrap();
        let mut m = MappedFile::open(dir.path().join("m.gguf")).unwrap();
        let r2 = crate::data::bench(&mut m, &offsets, 1000, false).unwrap();
        assert_eq!(r.checksum, r2.checksum, "tcp and the mapping read the same bytes");
        assert!(r.path.starts_with("tcp 127.0.0.1"));
        // the rdma hello without the feature, or without a port, is a clean refusal
        let (qp, addr, rkey, len) = (QpInfo { qpn: 1, psn: 2, gid: "00".repeat(16), lid: 0, mtu: 5 }, 0, 0, 0);
        let _ = (addr, rkey, len);
        let err = c.rt.block_on(c.stream.rdma_hello("m.gguf", 0, 4096, qp)).unwrap_err();
        assert!(err.to_string().contains("rdma"), "{err}");
    }
}
