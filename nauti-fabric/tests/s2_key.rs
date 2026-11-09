use nauti_fabric::{registry::*, sponsor::sponsor_for_key};
fn pk(b: u8) -> String { iroh::SecretKey::from_bytes(&[b; 32]).public().to_string() }
fn e(with: Option<&str>) -> Enrollment {
    let mut r = Registry::new(); let id = r.enroll(&pk(3), "n", &[], "home", Admission::Accepted).unwrap();
    let mut e = r.entries[&id].clone();
    e.sponsor = with.map(|k| SponsorSignature { sponsor_node_id: node_id_from_pubkey(k).unwrap(), signature: "ab".into() }); e
}
#[test] fn matching_key_returns_the_sponsor() { let k = pk(9); let x = e(Some(&k)); assert_eq!(sponsor_for_key(&x, &k).unwrap().signature, "ab"); }
#[test] fn uppercase_key_matches() { let k = pk(9); let x = e(Some(&k)); assert!(sponsor_for_key(&x, &k.to_uppercase()).is_ok()); }
#[test] fn no_sponsor_is_error() { assert!(sponsor_for_key(&e(None), &pk(9)).is_err()); }
#[test] fn other_key_is_error_naming_the_field() { let k = pk(9); let x = e(Some(&k)); let m = sponsor_for_key(&x, &pk(8)).unwrap_err().to_string(); assert!(m.contains("sponsor_node_id"), "{m}"); }
#[test] fn bad_key_text_is_error() { let k = pk(9); let x = e(Some(&k)); assert!(sponsor_for_key(&x, "nope").is_err()); }
