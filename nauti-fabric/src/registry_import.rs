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
    let s = key.trim();
    
    // Check for 64 hex characters FIRST (before base64 check)
    if s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        return crate::registry::normalize_pubkey(s);
    }
    
    // Strip optional "ed25519:" prefix
    let rest = s.strip_prefix("ed25519:").unwrap_or(s);
    
    // Strip trailing '=' padding
    let rest = rest.trim_end_matches('=');
    
    // Decode base64 manually
    let decoded = decode_base64_manual(rest)?;
    
    // Verify exactly 32 bytes
    if decoded.len() != 32 {
        return Err(RegistryError::Invalid("invalid key".into()));
    }
    
    // Convert to 64 lowercase hex
    Ok(decoded.iter().map(|b| format!("{b:02x}")).collect())
}

fn decode_base64_manual(input: &str) -> Result<Vec<u8>, RegistryError> {
    let mut bits = 0u32;
    let mut bit_count = 0;
    let mut output = Vec::new();
    
    for ch in input.chars() {
        let val = match ch {
            'A'..='Z' => ch as u32 - 'A' as u32,
            'a'..='z' => ch as u32 - 'a' as u32 + 26,
            '0'..='9' => ch as u32 - '0' as u32 + 52,
            '+' | '-' => 62,
            '/' | '_' => 63,
            _ => return Err(RegistryError::Invalid("invalid key".into())),
        };
        
        bits = (bits << 6) | val;
        bit_count += 6;
        
        while bit_count >= 8 {
            bit_count -= 8;
            output.push(((bits >> bit_count) & 0xFF) as u8);
        }
    }
    
    // If there are remaining bits, they should be zero (padding)
    if bit_count > 0 {
        if (bits & ((1 << bit_count) - 1)) != 0 {
            return Err(RegistryError::Invalid("invalid key".into()));
        }
    }
    
    Ok(output)
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
    let v: serde_json::Value = serde_json::from_str(choices_json)
        .map_err(|_| RegistryError::Invalid("invalid JSON".into()))?;
    
    let servers = v.get("servers")
        .and_then(|x| x.as_object())
        .ok_or_else(|| RegistryError::Invalid("invalid JSON".into()))?;
    
    let mut summary = ImportSummary::default();
    
    for (id, entry) in servers {
        // Extract fields
        let key = entry.get("public_key").and_then(|k| k.as_str()).unwrap_or("");
        let choice_str = entry.get("choice").and_then(|c| c.as_str()).unwrap_or("");
        let hostname = entry.get("hostname").and_then(|h| h.as_str()).unwrap_or("");
        let url = entry.get("url").and_then(|u| u.as_str()).unwrap_or("");
        
        // Skip if no key
        if key.is_empty() {
            summary.skipped_no_key += 1;
            continue;
        }
        
        // Parse choice
        let admission = match choice_str {
            "accepted" => Admission::Accepted,
            "ignored" => Admission::Ignored,
            _other => {
                summary.errors.push(format!("{}: invalid choice", id));
                continue;
            }
        };
        
        // Convert key
        let hex = match pubkey_hex_from_lemond(key) {
            Ok(h) => h,
            Err(e) => {
                summary.errors.push(format!("{}: {}", id, e));
                continue;
            }
        };
        
        // Enroll
        let urls = if url.is_empty() { Vec::new() } else { vec![url.to_string()] };
        match reg.enroll(&hex, hostname, &urls, "", admission) {
            Ok(_) => summary.imported += 1,
            Err(e) => summary.errors.push(format!("{}: {}", id, e)),
        }
    }
    
    Ok(summary)
}
