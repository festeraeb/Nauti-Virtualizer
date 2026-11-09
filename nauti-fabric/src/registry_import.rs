//! Import the Lemonade app's saved server choices (#1221 "found on this network") into the trust registry.
//!
//! The app keeps the user's Use / Ignore choices in `lan_servers.json` (under the OS config dir,
//! `lemonade/lan_servers.json`): an object `servers` mapping a choice id (e.g. "desk:13305") to
//! {"choice": "accepted" or "ignored", "hostname": "desk", "url": "http://192.168.4.10:13305", "updated": 1794200000,
//! "public_key": "ed25519:f6Y3aCO2va1Ht3WvqmkTo6J_VBcAoWrV5nEM8SOJ4vY"}.
//!
//! `public_key` is the server's lemond device key, as its `/api/v1/nexus/status` reports it, saved when the user
//! made the choice. Choices saved before the app recorded keys have none and cannot be enrolled (the registry is keyed
//! by the key). The key is only a claim the server made over plain HTTP: enrolling it records the user's decision about
//! that key; the fabric's own connection authentication is what proves a peer holds it.

use crate::registry::{Admission, Registry, RegistryError};

/// What an import did.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ImportSummary {
    /// Choices written to the registry (new or updated).
    pub imported: usize,
    /// Choices with no `public_key` (or an empty one): left out.
    pub skipped_no_key: usize,
    /// One line per choice that could not be imported (bad key, unknown choice), naming the choice id.
    pub errors: Vec<String>,
}

/// A lemond device key as 64 lowercase hex characters.
///
/// Accepts `ed25519:` followed by the key in base64 (standard `+/` or URL-safe `-_` alphabet, with or without `=`
/// padding), the same without the `ed25519:` prefix, or 64 hex characters (any case). Surrounding whitespace is
/// ignored. The decoded key must be exactly 32 bytes. Anything else is `RegistryError::Invalid` with a message saying
/// what was wrong. No external base64 crate: decode by hand.
pub fn pubkey_hex_from_lemond(key: &str) -> Result<String, RegistryError> {
    let _ = key;
    Err(RegistryError::Invalid("N2a pubkey_hex_from_lemond not built yet".into()))
}

/// Imports the choices in `choices_json` (the text of `lan_servers.json`) into `reg`.
///
/// For each entry of `servers` (any order): with a non-empty `public_key`, convert it with
/// [`pubkey_hex_from_lemond`] and call `reg.enroll(pubkey_hex, hostname, urls, "", admission)`, where `urls` is
/// `[url]` when `url` is not empty (else empty), and admission is `Accepted` for choice `"accepted"` and `Ignored` for
/// `"ignored"`. Missing `hostname`/`url` are empty strings. A bad key or another choice value adds one line to
/// `errors` (containing the entry's id, the key in `servers`) and the import goes on. Entries without a key count in
/// `skipped_no_key`. Text that is not JSON, or has no `servers` object, is `RegistryError::Invalid` and changes nothing.
/// An empty `servers` object is fine (all counts 0).
pub fn import_lemonade_choices(reg: &mut Registry, choices_json: &str) -> Result<ImportSummary, RegistryError> {
    let _ = (reg, choices_json, Admission::Accepted);
    Err(RegistryError::Invalid("N2a import_lemonade_choices not built yet".into()))
}
