//! N2a: importing the Lemonade app's saved choices into the trust registry.
//!     cargo test -p nauti-fabric --test registry_import

use nauti_fabric::registry::{Admission, Registry};
use nauti_fabric::registry_import::{import_lemonade_choices, pubkey_hex_from_lemond};

// A real lemond device key (from lemond's own test run) and its decoded bytes / registry node id.
const KEY_URL: &str = "ed25519:f6Y3aCO2va1Ht3WvqmkTo6J_VBcAoWrV5nEM8SOJ4vY";
const KEY_STD: &str = "ed25519:f6Y3aCO2va1Ht3WvqmkTo6J/VBcAoWrV5nEM8SOJ4vY=";
const HEX: &str = "7fa6376823b6bdad47b775afaa6913a3a27f541700a16ad5e6710cf12389e2f6";
const NODE_ID: &str = "3649f8b2d4f5f21a482dbf0177cbfc6f";

#[test]
fn key_forms_all_decode_to_the_same_hex() {
    assert_eq!(pubkey_hex_from_lemond(KEY_URL).unwrap(), HEX);
    assert_eq!(pubkey_hex_from_lemond(KEY_STD).unwrap(), HEX);
    assert_eq!(pubkey_hex_from_lemond("f6Y3aCO2va1Ht3WvqmkTo6J_VBcAoWrV5nEM8SOJ4vY").unwrap(), HEX);
    assert_eq!(pubkey_hex_from_lemond(&HEX.to_uppercase()).unwrap(), HEX);
    assert_eq!(pubkey_hex_from_lemond(&format!("  {KEY_URL}\n")).unwrap(), HEX);
}

#[test]
fn bad_keys_are_refused() {
    for bad in [
        "",
        "ed25519:",
        "ed25519:f6Y3aCO2va1Ht3WvqmkTo6J_VBcAoWrV5nEM8SOJ4v",      // 31.x bytes
        "ed25519:f6Y3aCO2va1Ht3WvqmkTo6J_VBcAoWrV5nEM8SOJ4vYAAAA", // 35 bytes
        "ed25519:f6Y3aCO2va1Ht3Wv*mkTo6J_VBcAoWrV5nEM8SOJ4vY",      // not base64
        "rsa:f6Y3aCO2va1Ht3WvqmkTo6J_VBcAoWrV5nEM8SOJ4vY",
        "7fa6376823b6bdad47b775afaa6913a3a27f541700a16ad5e6710cf12389e2",   // 62 hex
        "7fa6376823b6bdad47b775afaa6913a3a27f541700a16ad5e6710cf12389e2gg", // not hex, not base64 of 32 bytes
    ] {
        assert!(pubkey_hex_from_lemond(bad).is_err(), "{bad:?} should be refused");
    }
}

fn choices(entries: &str) -> String {
    format!(r#"{{"servers": {{{entries}}}}}"#)
}

#[test]
fn accepted_and_ignored_choices_are_enrolled() {
    let mut reg = Registry::new();
    let other = "ed25519:AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"; // bytes 0..31
    let text = choices(&format!(
        r#""desk:13305": {{"choice": "accepted", "hostname": "desk", "url": "http://192.168.4.10:13305", "updated": 1794200000, "public_key": "{KEY_URL}"}},
           "guest:13305": {{"choice": "ignored", "hostname": "guest", "url": "", "updated": 1794200001, "public_key": "{other}"}}"#
    ));
    let s = import_lemonade_choices(&mut reg, &text).unwrap();
    assert_eq!((s.imported, s.skipped_no_key, s.errors.len()), (2, 0, 0), "{s:?}");
    let desk = reg.entries.get(NODE_ID).expect("desk enrolled under its node id");
    assert_eq!(desk.pubkey, HEX);
    assert_eq!(desk.label, "desk");
    assert_eq!(desk.urls, vec!["http://192.168.4.10:13305".to_string()]);
    assert_eq!(desk.admission, Admission::Accepted);
    let guest = reg.entries.values().find(|e| e.label == "guest").expect("guest enrolled");
    assert_eq!(guest.pubkey, "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f");
    assert_eq!(guest.admission, Admission::Ignored);
    assert!(guest.urls.is_empty(), "an empty url is not recorded");
    assert!(reg.is_accepted(HEX) || reg.entries.get(NODE_ID).map(|e| e.admission) == Some(Admission::Accepted));
}

#[test]
fn choices_without_a_key_are_skipped_and_counted() {
    let mut reg = Registry::new();
    let text = choices(
        r#""old:13305": {"choice": "accepted", "hostname": "old", "url": "http://10.0.0.5:13305", "updated": 1},
           "blank:13305": {"choice": "accepted", "hostname": "blank", "url": "http://10.0.0.6:13305", "updated": 2, "public_key": ""}"#,
    );
    let s = import_lemonade_choices(&mut reg, &text).unwrap();
    assert_eq!((s.imported, s.skipped_no_key, s.errors.len()), (0, 2, 0), "{s:?}");
    assert!(reg.entries.is_empty());
}

#[test]
fn a_bad_entry_is_reported_and_the_rest_imported() {
    let mut reg = Registry::new();
    let text = choices(&format!(
        r#""broken:13305": {{"choice": "accepted", "hostname": "broken", "url": "", "public_key": "ed25519:nope"}},
           "weird:13305": {{"choice": "maybe", "hostname": "weird", "url": "", "public_key": "{KEY_STD}"}},
           "desk:13305": {{"choice": "accepted", "hostname": "desk", "url": "http://192.168.4.10:13305", "public_key": "{KEY_URL}"}}"#
    ));
    let s = import_lemonade_choices(&mut reg, &text).unwrap();
    assert_eq!(s.imported, 1, "{s:?}");
    assert_eq!(s.errors.len(), 2, "{s:?}");
    assert!(s.errors.iter().any(|e| e.contains("broken:13305")), "{s:?}");
    assert!(s.errors.iter().any(|e| e.contains("weird:13305")), "{s:?}");
    assert!(reg.entries.contains_key(NODE_ID));
}

#[test]
fn reimport_updates_one_record_per_key() {
    let mut reg = Registry::new();
    let first = choices(&format!(r#""desk:13305": {{"choice": "accepted", "hostname": "desk", "url": "http://a:13305", "public_key": "{KEY_URL}"}}"#));
    let second = choices(&format!(r#""desk:13305": {{"choice": "ignored", "hostname": "desk", "url": "http://b:13305", "public_key": "{KEY_STD}"}}"#));
    import_lemonade_choices(&mut reg, &first).unwrap();
    import_lemonade_choices(&mut reg, &second).unwrap();
    assert_eq!(reg.entries.len(), 1);
    let e = reg.entries.get(NODE_ID).unwrap();
    assert_eq!(e.admission, Admission::Ignored);
    assert_eq!(e.urls, vec!["http://b:13305".to_string()]);
}

#[test]
fn text_that_is_not_a_choices_file_changes_nothing() {
    let mut reg = Registry::new();
    for bad in ["", "not json", "[]", r#"{"servers": []}"#, r#"{"other": {}}"#] {
        assert!(import_lemonade_choices(&mut reg, bad).is_err(), "{bad:?} should be refused");
    }
    assert!(reg.entries.is_empty());
    let s = import_lemonade_choices(&mut reg, r#"{"servers": {}}"#).unwrap();
    assert_eq!((s.imported, s.skipped_no_key, s.errors.len()), (0, 0, 0));
}
