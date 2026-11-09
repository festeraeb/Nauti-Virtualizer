//! Small pieces of the sponsor-signature check.
use crate::registry::{Enrollment, RegistryError, SponsorSignature};

/// Decode exactly `N` bytes from a hex string.
pub fn hex_decode<const N: usize>(s: &str) -> Result<[u8; N], RegistryError> {
    let _ = s;
    todo!("implement hex_decode")
}

/// The enrollment's sponsor, if `sponsor_pubkey_hex` is the key it names.
pub fn sponsor_for_key<'a>(e: &'a Enrollment, sponsor_pubkey_hex: &str) -> Result<&'a SponsorSignature, RegistryError> {
    let _ = (e, sponsor_pubkey_hex);
    todo!("implement sponsor_for_key")
}

/// Check an Ed25519 signature over `msg`.
pub fn check_ed25519(key: &[u8; 32], msg: &[u8], sig: &[u8; 64]) -> Result<(), RegistryError> {
    let _ = (key, msg, sig);
    todo!("implement check_ed25519")
}
