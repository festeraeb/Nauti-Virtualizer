//! One relay configuration for every fabric endpoint.
//!
//! The control plane (`rpc.rs`) and the bulk data plane (`data_quic.rs`) each
//! bind Iroh endpoints, as servers and as clients. All four build sites take
//! their builder from [`fabric_endpoint`], so they always agree on the relay.
//!
//! `NAUTI_RELAY_URL` names the relay (an `iroh-relay` the operator runs, for
//! example `https://relay.example.org`). When it is set, the endpoint uses
//! that relay only: the public n0 relays are not used. When it is unset or
//! empty, the endpoint keeps the iroh default (the n0 relays) and logs one
//! warning, so a node without the variable keeps working as before. A value
//! that is not an http(s) URL is an error, never a silent fallback.
//!
//! Address lookup keeps the N0 preset (pkarr publish/resolve through n0
//! DNS); only the relay changes here.

use std::sync::Once;

use iroh::endpoint::{Builder, presets};
use iroh::{Endpoint, RelayMap, RelayMode, RelayUrl, SecretKey};
use tracing::warn;

use crate::rpc::RpcTransportError;

/// The environment variable that names the fabric relay.
pub const RELAY_URL_ENV: &str = "NAUTI_RELAY_URL";

static DEFAULT_RELAY_WARNING: Once = Once::new();

/// The relay a fabric endpoint uses, from the value of [`RELAY_URL_ENV`].
///
/// `None` (or an empty value) means the iroh default relays; `Some(url)`
/// means a relay map with that relay alone.
pub fn relay_url_from(value: Option<&str>) -> Result<Option<RelayUrl>, RpcTransportError> {
    let Some(text) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    if !(text.starts_with("https://") || text.starts_with("http://")) {
        return Err(RpcTransportError::Iroh(format!(
            "{RELAY_URL_ENV}={text:?} is not an http(s) relay URL"
        )));
    }
    let url: RelayUrl = text
        .parse()
        .map_err(|e| RpcTransportError::Iroh(format!("{RELAY_URL_ENV}={text:?} is not a relay URL: {e}")))?;
    Ok(Some(url))
}

/// The relay mode for an optional custom relay: `Custom` with that relay
/// alone, or `None` to keep the preset's default.
pub fn relay_mode_for(url: Option<RelayUrl>) -> Option<RelayMode> {
    url.map(|u| RelayMode::Custom(RelayMap::from(u)))
}

/// An endpoint builder with the N0 preset and the relay from `relay_url`
/// (the value of [`RELAY_URL_ENV`]), plus `secret` as the endpoint identity
/// when given. An unset relay keeps the n0 default with a one-time warning.
pub fn fabric_endpoint_with(
    relay_url: Option<&str>,
    secret: Option<SecretKey>,
) -> Result<Builder, RpcTransportError> {
    let mut builder = Endpoint::builder(presets::N0);
    match relay_mode_for(relay_url_from(relay_url)?) {
        Some(mode) => builder = builder.relay_mode(mode),
        None => DEFAULT_RELAY_WARNING.call_once(|| {
            warn!(
                "{RELAY_URL_ENV} is not set: fabric endpoints fall back to the public n0 relays; \
                 set it to your own iroh-relay"
            );
        }),
    }
    if let Some(key) = secret {
        builder = builder.secret_key(key);
    }
    Ok(builder)
}

/// [`fabric_endpoint_with`] with the relay from the environment.
pub fn fabric_endpoint(secret: Option<SecretKey>) -> Result<Builder, RpcTransportError> {
    let value = std::env::var(RELAY_URL_ENV).ok();
    fabric_endpoint_with(value.as_deref(), secret)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n0_urls() -> Vec<RelayUrl> {
        RelayMode::Default.relay_map().urls::<Vec<_>>()
    }

    #[test]
    fn unset_or_empty_keeps_the_default() {
        assert!(relay_url_from(None).unwrap().is_none());
        assert!(relay_url_from(Some("")).unwrap().is_none());
        assert!(relay_url_from(Some("   ")).unwrap().is_none());
        assert!(relay_mode_for(None).is_none());
        // The builder still builds: an unset variable must not stop a node.
        assert!(fabric_endpoint_with(None, None).is_ok());
    }

    #[test]
    fn a_custom_url_is_the_only_relay() {
        let url = relay_url_from(Some("https://relay.example.org")).unwrap().unwrap();
        let Some(RelayMode::Custom(map)) = relay_mode_for(Some(url.clone())) else {
            panic!("expected a custom relay mode");
        };
        let urls: Vec<RelayUrl> = map.urls();
        assert_eq!(urls, vec![url]);
        let n0 = n0_urls();
        assert!(!n0.is_empty());
        assert!(urls.iter().all(|u| !n0.contains(u)));
    }

    #[test]
    fn an_invalid_value_is_an_error_not_a_fallback() {
        assert!(relay_url_from(Some("relay.example.org")).is_err());
        assert!(relay_url_from(Some("ftp://relay.example.org")).is_err());
        assert!(relay_url_from(Some("https://")).is_err());
        assert!(fabric_endpoint_with(Some("not a url"), None).is_err());
    }
}
