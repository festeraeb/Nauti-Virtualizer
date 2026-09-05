//! Agent RPC contract and Iroh/QUIC transport for controlling a remote [`Fabric`].
//!
//! This module defines the wire contract used by two cooperating processes (an
//! "agent" hosting a [`Fabric`] and a "controller" driving it remotely) and
//! implements that contract over authenticated Iroh/QUIC connections.
//!
//! # Authentication
//!
//! Iroh endpoints are identified by an Ed25519 keypair (`EndpointId` /
//! `PublicKey`) exchanged and verified during the QUIC/TLS handshake. Every
//! [`RpcRequest`] therefore arrives over a connection whose remote identity is
//! already cryptographically established; the handler records
//! `connection.remote_id()` so callers can audit which endpoint issued a
//! command.
//!
//! # Framing
//!
//! Requests and responses are JSON-encoded ([`serde_json`]) and sent on a
//! single bidirectional QUIC stream as `u32` little-endian length-prefixed
//! frames. Idempotency for lease/attach/release operations comes directly
//! from the underlying [`Fabric`] semantics (leasing an already-leased
//! resource or releasing an unknown lease both return stable errors rather
//! than mutating state twice).

use std::sync::Arc;
use std::time::Duration;

use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr, endpoint::presets};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::info;

use crate::{Attachment, Fabric, FabricError, Lease, Resource, ResourceRequest};

/// ALPN identifying the nauti-fabric agent RPC protocol.
pub const ALPN: &[u8] = b"nauti-fabric/rpc/1";

/// Maximum size (in bytes) accepted for a single RPC frame, guarding against
/// unbounded allocation from a malformed or hostile peer.
const MAX_FRAME_BYTES: u32 = 16 * 1024 * 1024;

/// Requests a controller may issue against a remote [`Fabric`].
#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum RpcRequest {
    /// Liveness/handshake check; always answered with [`RpcResponse::Pong`].
    Ping,
    /// List every resource currently registered with the fabric.
    Inventory,
    /// Query for resources matching a [`ResourceRequest`].
    FindAvailable(ResourceRequest),
    /// Attempt to take an exclusive, time-bounded lease on a resource.
    LeaseExclusive {
        resource_id: String,
        owner: String,
        ttl_secs: u64,
    },
    /// Attach an adapter to a previously granted lease.
    Attach { adapter: String, lease: Lease },
    /// Release a previously granted lease.
    Release(Lease),
}

/// Responses returned for each [`RpcRequest`] variant.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum RpcResponse {
    Pong,
    Inventory(Vec<Resource>),
    FindAvailable(Vec<Resource>),
    Leased(Lease),
    Attached(Attachment),
    Released,
    Error(RpcError),
}

/// Wire-safe representation of a [`FabricError`], since `thiserror` errors
/// don't themselves implement `Serialize`/`Deserialize`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RpcError {
    pub message: String,
}

impl From<FabricError> for RpcError {
    fn from(error: FabricError) -> Self {
        Self { message: error.to_string() }
    }
}

impl<T> From<Result<T, FabricError>> for RpcResponse
where
    RpcResponse: From<T>,
{
    fn from(result: Result<T, FabricError>) -> Self {
        match result {
            Ok(value) => RpcResponse::from(value),
            Err(error) => RpcResponse::Error(error.into()),
        }
    }
}

impl From<Lease> for RpcResponse {
    fn from(lease: Lease) -> Self {
        RpcResponse::Leased(lease)
    }
}

impl From<Attachment> for RpcResponse {
    fn from(attachment: Attachment) -> Self {
        RpcResponse::Attached(attachment)
    }
}

impl From<()> for RpcResponse {
    fn from(_: ()) -> Self {
        RpcResponse::Released
    }
}

/// Errors that can occur at the transport layer, distinct from
/// application-level [`RpcError`]s that are carried inside a successful
/// response.
#[derive(Debug, thiserror::Error)]
pub enum RpcTransportError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame of {0} bytes exceeds maximum of {MAX_FRAME_BYTES}")]
    FrameTooLarge(u32),
    #[error("connection closed before a complete frame was received")]
    ConnectionClosed,
    #[error("failed to encode/decode JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("iroh transport error: {0}")]
    Iroh(String),
}

async fn write_frame<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    value: &impl Serialize,
) -> Result<(), RpcTransportError> {
    let bytes = serde_json::to_vec(value)?;
    let len = u32::try_from(bytes.len()).map_err(|_| RpcTransportError::FrameTooLarge(u32::MAX))?;
    writer.write_all(&len.to_le_bytes()).await?;
    writer.write_all(&bytes).await?;
    Ok(())
}

async fn read_frame<R: AsyncReadExt + Unpin, T: for<'de> Deserialize<'de>>(
    reader: &mut R,
) -> Result<T, RpcTransportError> {
    let mut len_bytes = [0u8; 4];
    reader.read_exact(&mut len_bytes).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            RpcTransportError::ConnectionClosed
        } else {
            RpcTransportError::Io(error)
        }
    })?;
    let len = u32::from_le_bytes(len_bytes);
    if len > MAX_FRAME_BYTES {
        return Err(RpcTransportError::FrameTooLarge(len));
    }
    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf).await?;
    Ok(serde_json::from_slice(&buf)?)
}

/// Dispatches a single [`RpcRequest`] against the fabric, returning the
/// corresponding [`RpcResponse`].
fn dispatch(fabric: &Fabric, request: RpcRequest) -> RpcResponse {
    match request {
        RpcRequest::Ping => RpcResponse::Pong,
        RpcRequest::Inventory => RpcResponse::Inventory(fabric.resources()),
        RpcRequest::FindAvailable(query) => RpcResponse::FindAvailable(fabric.find_available(&query)),
        RpcRequest::LeaseExclusive { resource_id, owner, ttl_secs } => {
            RpcResponse::from(fabric.lease_exclusive(&resource_id, owner, Duration::from_secs(ttl_secs)))
        }
        RpcRequest::Attach { adapter, lease } => RpcResponse::from(fabric.attach(&adapter, &lease)),
        RpcRequest::Release(lease) => RpcResponse::from(fabric.release(&lease)),
    }
}

/// Iroh [`ProtocolHandler`] that serves [`RpcRequest`]s against a shared
/// [`Fabric`] over the nauti agent [`ALPN`].
#[derive(Clone)]
pub struct FabricAgent {
    fabric: Arc<Fabric>,
}

impl std::fmt::Debug for FabricAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FabricAgent").finish_non_exhaustive()
    }
}

impl FabricAgent {
    pub fn new(fabric: Arc<Fabric>) -> Self {
        Self { fabric }
    }
}

impl ProtocolHandler for FabricAgent {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let remote_id = connection.remote_id();
        info!(%remote_id, "nauti agent: accepted rpc connection");

        let (mut send, mut recv) = connection.accept_bi().await?;

        loop {
            let request: RpcRequest = match read_frame(&mut recv).await {
                Ok(request) => request,
                Err(RpcTransportError::ConnectionClosed) => break,
                Err(error) => return Err(AcceptError::from_err(error)),
            };
            let response = dispatch(&self.fabric, request);
            write_frame(&mut send, &response).await.map_err(AcceptError::from_err)?;
        }

        send.finish()?;
        connection.closed().await;
        info!(%remote_id, "nauti agent: rpc connection closed");
        Ok(())
    }
}

/// Starts serving the fabric agent RPC protocol on a freshly bound Iroh
/// endpoint and returns the running [`Router`] plus the [`EndpointAddr`]
/// callers must share (out-of-band) with a controller so it can connect.
pub async fn serve(fabric: Arc<Fabric>) -> Result<(Router, EndpointAddr), RpcTransportError> {
    let endpoint = Endpoint::bind(presets::N0)
        .await
        .map_err(|error| RpcTransportError::Iroh(error.to_string()))?;
    endpoint.online().await;
    let addr = endpoint.addr();
    let router = Router::builder(endpoint).accept(ALPN, FabricAgent::new(fabric)).spawn();
    Ok((router, addr))
}

/// A connected controller session, able to issue [`RpcRequest`]s against a
/// remote agent and await matching [`RpcResponse`]s.
pub struct AgentClient {
    endpoint: Endpoint,
    connection: Connection,
    send: iroh::endpoint::SendStream,
    recv: iroh::endpoint::RecvStream,
}

impl AgentClient {
    /// Connects to a remote fabric agent advertised at `addr`.
    pub async fn connect(addr: EndpointAddr) -> Result<Self, RpcTransportError> {
        let endpoint = Endpoint::bind(presets::N0)
            .await
            .map_err(|error| RpcTransportError::Iroh(error.to_string()))?;
        let connection = endpoint
            .connect(addr, ALPN)
            .await
            .map_err(|error| RpcTransportError::Iroh(error.to_string()))?;
        let (send, recv) = connection
            .open_bi()
            .await
            .map_err(|error| RpcTransportError::Iroh(error.to_string()))?;
        Ok(Self { endpoint, connection, send, recv })
    }

    /// Sends a request and awaits its response on the shared bidi stream.
    pub async fn call(&mut self, request: RpcRequest) -> Result<RpcResponse, RpcTransportError> {
        write_frame(&mut self.send, &request).await?;
        read_frame(&mut self.recv).await
    }

    /// Gracefully closes the RPC stream and underlying connection/endpoint.
    pub async fn close(mut self) -> Result<(), RpcTransportError> {
        self.send.finish().map_err(|error| RpcTransportError::Iroh(error.to_string()))?;
        self.connection.close(0u32.into(), b"bye");
        self.endpoint.close().await;
        Ok(())
    }
}
