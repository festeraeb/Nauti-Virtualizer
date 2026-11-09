use nauti_fabric::registry::*;

fn sponsor() -> iroh::SecretKey { iroh::SecretKey::from_bytes(&[9u8; 32]) }
fn hex(b: &[u8]) -> String { b.iter().map(|x| format!("{x:02x}")).collect() }

fn signed() -> Enrollment {
    let mut r = Registry::new();
    let pk = iroh::SecretKey::from_bytes(&[3u8; 32]).public().to_string();
    let id = r.enroll(&pk, "n", &["http://b:1".into(), "http://a:1".into()], "home", Admission::Accepted).unwrap();
    let mut e = r.entries[&id].clone();
    let s = sponsor();
    let sig = s.sign(&e.canonical_bytes());
    e.sponsor = Some(SponsorSignature {
        sponsor_node_id: node_id_from_pubkey(&s.public().to_string()).unwrap(),
        signature: hex(&sig.to_bytes()),
    });
    e
}
fn spk() -> String { sponsor().public().to_string() }

#[test] fn valid_signature_verifies() { verify_sponsor(&signed(), &spk()).unwrap(); }
#[test] fn uppercase_sponsor_key_verifies() { verify_sponsor(&signed(), &spk().to_uppercase()).unwrap(); }
#[test] fn url_order_does_not_matter() { let mut e = signed(); e.urls.reverse(); verify_sponsor(&e, &spk()).unwrap(); }
#[test] fn label_is_not_signed() { let mut e = signed(); e.label = "renamed".into(); verify_sponsor(&e, &spk()).unwrap(); }
#[test] fn changed_url_fails() { let mut e = signed(); e.urls.push("http://evil:1".into()); assert!(verify_sponsor(&e, &spk()).is_err()); }
#[test] fn changed_zone_fails() { let mut e = signed(); e.zone = "away".into(); assert!(verify_sponsor(&e, &spk()).is_err()); }
#[test] fn no_sponsor_is_an_error() { let mut e = signed(); e.sponsor = None; assert!(verify_sponsor(&e, &spk()).is_err()); }
#[test] fn wrong_key_is_rejected_before_verifying() {
    let other = iroh::SecretKey::from_bytes(&[8u8; 32]).public().to_string();
    let err = verify_sponsor(&signed(), &other).unwrap_err().to_string();
    assert!(err.contains("sponsor_node_id"), "{err}");
}
#[test] fn forged_signature_with_the_right_key_fails() {
    let mut e = signed();
    let forger = iroh::SecretKey::from_bytes(&[8u8; 32]);
    e.sponsor.as_mut().unwrap().signature = hex(&forger.sign(&e.canonical_bytes()).to_bytes());
    assert!(verify_sponsor(&e, &spk()).is_err());
}
#[test] fn short_signature_is_an_error_not_a_panic() { let mut e = signed(); e.sponsor.as_mut().unwrap().signature = "abcd".into(); assert!(verify_sponsor(&e, &spk()).is_err()); }
#[test] fn non_hex_signature_is_an_error_not_a_panic() { let mut e = signed(); e.sponsor.as_mut().unwrap().signature = "zz".repeat(64); assert!(verify_sponsor(&e, &spk()).is_err()); }
