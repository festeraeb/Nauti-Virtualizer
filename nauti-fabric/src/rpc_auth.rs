//! Application-layer authorization for the agent RPC protocol.
//!
//! ## Why this layer exists
//!
//! The Iroh/QUIC transport already authenticates the *peer*: every connection
//! carries an Ed25519 endpoint identity verified during the QUIC/TLS handshake.
//! For a single-operator, single-LAN deployment that is the only authentication
//! most deployments need.
//!
//! For everything else, an operator may want an additional *application-layer*
//! check on top of transport authentication. Examples:
//!
//! - **TTL bounds:** reject `LeaseExclusive` requests whose TTL is implausibly
//!   long (a misbehaving controller asking for a 30-day lease).
//! - **OAuth2 bearer tokens:** require the request to carry a valid JWT for
//!   the configured audience.
//! - **Shared secret:** require an `X-Nauti-Token: <secret>` header on every
//!   request, validated against a value loaded from a file or env var.
//! - **Custom logic:** mTLS pinning to a private CA, an in-memory allowlist of
//!   endpoint IDs, a remote-policy lookup, etc.
//!
//! This module provides the plug-in spot: a single [`AuthProvider`] trait that
//! every [`RpcRequest`](crate::rpc::RpcRequest) flows through before
//! dispatch. The default impl is [`NoAuth`], which accepts every request and
//! is always compiled. Other impls are gated behind Cargo features and only
//! pulled in when an operator explicitly opts in.
//!
//! ## Threat model
//!
//! This is *not* a substitute for transport security. If an attacker can
//! MITM the QUIC connection, they can present a forged endpoint identity
//! and this layer is irrelevant. The right production setup is mTLS (or
//! Iroh's relay + endpoint identity) at the transport, plus an
//! [`AuthProvider`] for *authorization* on top: "this authenticated peer
//! is allowed to ask for a lease on this resource, with this TTL, on
//! behalf of this owner."

#[cfg(feature = "auth-ttl")]
use std::time::Duration;

/// Information about the connection a request arrived on. Passed to
/// [`AuthProvider::authorize`] so the impl can make policy decisions.
#[derive(Clone, Debug)]
pub struct RequestContext {
    /// The Iroh endpoint id of the remote peer, verified by QUIC/TLS. This
    /// is the peer's *public* identity, not a per-request credential.
    pub remote_endpoint_id: String,
    /// The local endpoint id of the agent that received the request. Useful
    /// for multi-agent deployments where the same auth impl might want to
    /// behave differently per agent.
    pub local_endpoint_id: String,
}

/// Reasons an [`AuthProvider`] can reject a request. Carried in the
/// [`AuthError`] returned from [`AuthProvider::authorize`] and turned into an
/// `RpcResponse::Error(AuthRejected)` by the RPC dispatch layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthRejection {
    /// The request violates a configured policy. `reason` is a short,
    /// human-readable explanation suitable for logging; it is *not* a secret
    /// and is safe to surface to the operator on either side of the wire.
    PolicyViolation { reason: String },
    /// The request requires an auth feature that the agent is not configured
    /// with. Returned by stub impls (e.g. `OAuth2Bearer` without a token
    /// URL configured) so the operator gets a clear "you have not finished
    /// setting this up" message instead of a silent pass.
    NotConfigured { adapter: String, hint: String },
    /// A credential was supplied but failed validation (bad signature,
    /// expired, wrong audience, etc.). Distinct from `NotConfigured` so the
    /// operator can tell "I never set this up" from "I set it up wrong."
    InvalidCredential { reason: String },
}

/// Errors from the auth layer. Serializes to an `RpcError`-shaped wire message
/// so the controller learns *that* the request was rejected; the structured
/// `AuthRejection` is logged server-side.
#[derive(Clone, Debug, thiserror::Error)]
#[error("auth rejected: {0:?}")]
pub struct AuthError(pub AuthRejection);

impl AuthError {
    pub fn policy(reason: impl Into<String>) -> Self {
        Self(AuthRejection::PolicyViolation { reason: reason.into() })
    }
    pub fn not_configured(adapter: impl Into<String>, hint: impl Into<String>) -> Self {
        Self(AuthRejection::NotConfigured { adapter: adapter.into(), hint: hint.into() })
    }
    pub fn invalid_credential(reason: impl Into<String>) -> Self {
        Self(AuthRejection::InvalidCredential { reason: reason.into() })
    }
}

impl From<AuthRejection> for AuthError {
    fn from(rejection: AuthRejection) -> Self {
        Self(rejection)
    }
}

/// The plug-in spot for application-layer authorization.
///
/// Every request handled by [`crate::rpc::serve`] flows through
/// `auth.authorize(&ctx, &request)` before [`crate::rpc::dispatch`]. The
/// default impl is [`NoAuth`], which accepts every request.
///
/// ## Implementing
///
/// - **Synchronous and infallible to the point of decision.** The trait is
///   not async; a real impl that needs I/O (OAuth2 token validation, a
///   remote policy lookup) should use a blocking client or, more idiomatically,
///   cache the result. The fabric is single-tenant per agent process.
/// - **Cheap.** This runs on every request; an impl that does a network
///   round-trip per call is wrong.
/// - **Honest about what it can and cannot check.** Return
///   `AuthRejection::NotConfigured` if you are an unimplemented stub; return
///   `AuthRejection::InvalidCredential` for a present-but-bad credential;
///   return `AuthRejection::PolicyViolation` for everything else. Do not
///   silently pass through "I don't know" requests.
pub trait AuthProvider: Send + Sync {
    /// Short, human-readable name of this provider, used in capability
    /// reports and logs. Examples: `"none"`, `"ttl-bound"`,
    /// `"oauth2-bearer"`.
    fn name(&self) -> &str;

    /// Inspect a request and return `Ok(())` to allow it or `Err(AuthError)`
    /// to reject it. The provider may also use this hook to log the
    /// decision (e.g. an audit line per request).
    fn authorize(
        &self,
        ctx: &RequestContext,
        request: &crate::rpc::RpcRequest,
    ) -> Result<(), AuthError>;
}

// ---------------------------------------------------------------------------
// NoAuth (default, always compiled)
// ---------------------------------------------------------------------------

/// The default [`AuthProvider`]. Accepts every request. The fabric relies on
/// Iroh/QUIC's transport authentication (verified endpoint identity) for the
/// single-LAN, single-operator case.
#[derive(Clone, Debug, Default)]
pub struct NoAuth;

impl AuthProvider for NoAuth {
    fn name(&self) -> &str {
        "none"
    }

    fn authorize(
        &self,
        _ctx: &RequestContext,
        _request: &crate::rpc::RpcRequest,
    ) -> Result<(), AuthError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// TtlBound (feature: auth-ttl)
// ---------------------------------------------------------------------------

/// Rejects `LeaseExclusive` requests whose TTL is outside a configured
/// `[min, max]` window. Accepts every other request. Useful as a first
/// defense against a misbehaving or compromised controller asking for
/// day-long leases on shared resources.
///
/// Compiled only with `--features auth-ttl`.
#[cfg(feature = "auth-ttl")]
#[derive(Clone, Debug)]
pub struct TtlBound {
    pub min: Duration,
    pub max: Duration,
}

#[cfg(feature = "auth-ttl")]
impl Default for TtlBound {
    fn default() -> Self {
        // Reasonable defaults: 1 second minimum (rejects zero/negative
        // requests that would be effectively permanent), 1 hour maximum
        // (an operator with a real long-lived job can configure a higher
        // max explicitly).
        Self { min: Duration::from_secs(1), max: Duration::from_secs(3600) }
    }
}

#[cfg(feature = "auth-ttl")]
impl AuthProvider for TtlBound {
    fn name(&self) -> &str {
        "ttl-bound"
    }

    fn authorize(
        &self,
        _ctx: &RequestContext,
        request: &crate::rpc::RpcRequest,
    ) -> Result<(), AuthError> {
        if let crate::rpc::RpcRequest::LeaseExclusive { ttl_secs, owner, .. } = request {
            let ttl = Duration::from_secs(*ttl_secs);
            if ttl < self.min {
                return Err(AuthError::policy(format!(
                    "lease ttl {ttl:?} for owner {owner:?} is below minimum {:?}",
                    self.min
                )));
            }
            if ttl > self.max {
                return Err(AuthError::policy(format!(
                    "lease ttl {ttl:?} for owner {owner:?} exceeds maximum {:?}",
                    self.max
                )));
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// OAuth2Bearer (feature: auth-oauth) — STUB
// ---------------------------------------------------------------------------

/// Stub for an OAuth2 bearer-token validator. The contract is documented; the
/// actual token-validation logic is a follow-up. With the current stub, every
/// request is rejected with `AuthRejection::NotConfigured` so a misconfigured
/// deployment fails closed instead of silently passing.
///
/// To finish this impl: load a JWKS URL and expected audience from
/// `nauti-agent.toml`, fetch the JWKS, validate the `Authorization: Bearer …`
/// header's JWT signature/exp/aud, and accept the request. That's a real
/// piece of work; left as a follow-up.
#[cfg(feature = "auth-oauth")]
#[derive(Clone, Debug, Default)]
pub struct OAuth2Bearer {
    /// The JWKS URL to fetch the public keys from. If `None`, every request
    /// is rejected with `NotConfigured`.
    pub jwks_url: Option<String>,
    /// The expected `aud` claim. If `None`, every request is rejected.
    pub audience: Option<String>,
}

#[cfg(feature = "auth-oauth")]
impl AuthProvider for OAuth2Bearer {
    fn name(&self) -> &str {
        "oauth2-bearer"
    }

    fn authorize(
        &self,
        _ctx: &RequestContext,
        _request: &crate::rpc::RpcRequest,
    ) -> Result<(), AuthError> {
        Err(AuthError::not_configured(
            "oauth2-bearer",
            match (self.jwks_url.as_ref(), self.audience.as_ref()) {
                (None, _) => "set [auth] jwks_url in nauti-agent.toml",
                (_, None) => "set [auth] oauth.audience in nauti-agent.toml",
                (Some(_), Some(_)) => {
                    "token validation not yet implemented; this is a documented stub"
                }
            },
        ))
    }
}

// ---------------------------------------------------------------------------
// SharedSecret (feature: auth-shared-secret) — STUB
// ---------------------------------------------------------------------------

/// Stub for a static shared-secret check. The operator writes a secret to a
/// file (mode 0600), the agent reads it on startup, and every request must
/// carry `X-Nauti-Token: <secret>` in its envelope metadata. With the current
/// stub, every request is rejected with `NotConfigured`; finishing the impl
/// is a follow-up that needs the request envelope to carry a header map
/// (currently the `RpcRequest` enum has no per-request metadata field).
#[cfg(feature = "auth-shared-secret")]
#[derive(Clone, Debug, Default)]
pub struct SharedSecret {
    /// The expected token value. If `None`, every request is rejected.
    pub token: Option<String>,
}

#[cfg(feature = "auth-shared-secret")]
impl AuthProvider for SharedSecret {
    fn name(&self) -> &str {
        "shared-secret"
    }

    fn authorize(
        &self,
        _ctx: &RequestContext,
        _request: &crate::rpc::RpcRequest,
    ) -> Result<(), AuthError> {
        Err(AuthError::not_configured(
            "shared-secret",
            match self.token.as_ref() {
                None => "set [auth] shared_secret.file in nauti-agent.toml",
                Some(_) => {
                    "header check not yet implemented; needs X-Nauti-Token on RpcRequest"
                }
            },
        ))
    }
}

// ---------------------------------------------------------------------------
// Display
// ---------------------------------------------------------------------------

impl std::fmt::Display for AuthRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthRejection::PolicyViolation { reason } => {
                write!(formatter, "policy violation: {reason}")
            }
            AuthRejection::NotConfigured { adapter, hint } => {
                write!(formatter, "{adapter} not configured: {hint}")
            }
            AuthRejection::InvalidCredential { reason } => {
                write!(formatter, "invalid credential: {reason}")
            }
        }
    }
}

impl From<AuthError> for crate::rpc::RpcError {
    fn from(error: AuthError) -> Self {
        Self { message: error.0.to_string() }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::RpcRequest;

    fn ctx() -> RequestContext {
        RequestContext {
            remote_endpoint_id: "remote-peer".into(),
            local_endpoint_id: "local-agent".into(),
        }
    }

    fn lease_request(ttl_secs: u64) -> RpcRequest {
        RpcRequest::LeaseExclusive {
            resource_id: "gpu.0".into(),
            owner: "test".into(),
            ttl_secs,
        }
    }

    #[test]
    fn no_auth_accepts_every_request() {
        let auth = NoAuth;
        assert_eq!(auth.name(), "none");
        auth.authorize(&ctx(), &RpcRequest::Ping).unwrap();
        auth.authorize(&ctx(), &lease_request(0)).unwrap();
        auth.authorize(&ctx(), &lease_request(u64::MAX)).unwrap();
    }

    #[test]
    fn auth_error_displays_usefully() {
        let err = AuthError::policy("ttl too long");
        assert_eq!(err.to_string(), "auth rejected: PolicyViolation { reason: \"ttl too long\" }");
        let rendered = err.0.to_string();
        assert!(rendered.contains("policy violation"));
        assert!(rendered.contains("ttl too long"));
    }

    #[test]
    fn auth_rejection_distinguishes_not_configured_from_invalid_credential() {
        let not_cfg = AuthError::not_configured("oauth2-bearer", "set jwks_url");
        let invalid = AuthError::invalid_credential("expired");
        assert!(matches!(not_cfg.0, AuthRejection::NotConfigured { .. }));
        assert!(matches!(invalid.0, AuthRejection::InvalidCredential { .. }));
    }

    #[cfg(feature = "auth-ttl")]
    #[test]
    fn ttl_bound_default_rejects_zero_and_huge() {
        let auth = TtlBound::default();
        assert_eq!(auth.name(), "ttl-bound");
        // 0 seconds is below the 1-second minimum
        let err = auth.authorize(&ctx(), &lease_request(0)).unwrap_err();
        assert!(matches!(err.0, AuthRejection::PolicyViolation { .. }));
        // 1 day exceeds the 1-hour default maximum
        let err = auth
            .authorize(&ctx(), &lease_request(24 * 60 * 60))
            .unwrap_err();
        assert!(matches!(err.0, AuthRejection::PolicyViolation { .. }));
    }

    #[cfg(feature = "auth-ttl")]
    #[test]
    fn ttl_bound_default_accepts_a_normal_request() {
        let auth = TtlBound::default();
        // A 60-second lease is inside [1s, 1h].
        auth.authorize(&ctx(), &lease_request(60)).unwrap();
        // A 1-second lease is at the minimum boundary and should be accepted.
        auth.authorize(&ctx(), &lease_request(1)).unwrap();
        // A 1-hour lease is at the maximum boundary and should be accepted.
        auth.authorize(&ctx(), &lease_request(3600)).unwrap();
    }

    #[cfg(feature = "auth-ttl")]
    #[test]
    fn ttl_bound_ignores_non_lease_requests() {
        // TTL bounds only constrain LeaseExclusive. Other requests pass
        // through untouched so a future RPC variant cannot accidentally
        // be blocked.
        let auth = TtlBound { min: Duration::from_secs(60), max: Duration::from_secs(60) };
        auth.authorize(&ctx(), &RpcRequest::Ping).unwrap();
        auth.authorize(&ctx(), &RpcRequest::Inventory).unwrap();
        auth.authorize(&ctx(), &RpcRequest::Release(crate::Lease {
            id: 1, resource_id: "x".into(), owner: "y".into(),
        })).unwrap();
    }

    #[cfg(feature = "auth-ttl")]
    #[test]
    fn ttl_bound_configured_window_is_respected() {
        let auth = TtlBound { min: Duration::from_secs(5), max: Duration::from_secs(30) };
        // Below the configured minimum.
        assert!(auth.authorize(&ctx(), &lease_request(4)).is_err());
        // At and above the minimum, at and below the maximum: all accepted.
        auth.authorize(&ctx(), &lease_request(5)).unwrap();
        auth.authorize(&ctx(), &lease_request(30)).unwrap();
        // Above the configured maximum.
        assert!(auth.authorize(&ctx(), &lease_request(31)).is_err());
    }

    #[cfg(feature = "auth-oauth")]
    #[test]
    fn oauth_stub_rejects_every_request_when_not_configured() {
        let auth = OAuth2Bearer::default();
        assert_eq!(auth.name(), "oauth2-bearer");
        let err = auth.authorize(&ctx(), &RpcRequest::Ping).unwrap_err();
        match err.0 {
            AuthRejection::NotConfigured { adapter, hint } => {
                assert_eq!(adapter, "oauth2-bearer");
                assert!(hint.contains("jwks_url"));
            }
            other => panic!("expected NotConfigured, got {other:?}"),
        }
    }

    #[cfg(feature = "auth-oauth")]
    #[test]
    fn oauth_stub_with_partial_config_still_rejects() {
        // jwks_url set but audience missing: still a misconfiguration.
        let auth = OAuth2Bearer { jwks_url: Some("https://example.invalid/.well-known/jwks.json".into()), audience: None };
        let err = auth.authorize(&ctx(), &RpcRequest::Ping).unwrap_err();
        match err.0 {
            AuthRejection::NotConfigured { hint, .. } => {
                assert!(hint.contains("audience"));
            }
            other => panic!("expected NotConfigured, got {other:?}"),
        }
    }

    #[cfg(feature = "auth-shared-secret")]
    #[test]
    fn shared_secret_stub_rejects_every_request_when_token_missing() {
        let auth = SharedSecret::default();
        assert_eq!(auth.name(), "shared-secret");
        let err = auth.authorize(&ctx(), &RpcRequest::Ping).unwrap_err();
        match err.0 {
            AuthRejection::NotConfigured { adapter, hint } => {
                assert_eq!(adapter, "shared-secret");
                assert!(hint.contains("file"));
            }
            other => panic!("expected NotConfigured, got {other:?}"),
        }
    }

    #[cfg(feature = "auth-shared-secret")]
    #[test]
    fn shared_secret_stub_with_token_set_still_rejects_as_unimplemented() {
        let auth = SharedSecret { token: Some("hunter2".into()) };
        let err = auth.authorize(&ctx(), &RpcRequest::Ping).unwrap_err();
        match err.0 {
            AuthRejection::NotConfigured { hint, .. } => {
                assert!(hint.contains("not yet implemented") || hint.contains("RpcRequest"));
            }
            other => panic!("expected NotConfigured (stub), got {other:?}"),
        }
    }
}
