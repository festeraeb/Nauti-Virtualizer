use nauti_fabric::registry::*;

const A: &str = "0101010101010101010101010101010101010101010101010101010101010101";
const B: &str = "0202020202020202020202020202020202020202020202020202020202020202";
fn reg(pk: &str, label: &str, urls: &[&str], adm: Admission) -> (Registry, String) {
    let mut r = Registry::new();
    let u: Vec<String> = urls.iter().map(|s| s.to_string()).collect();
    let id = r.enroll(pk, label, &u, "home", adm).unwrap();
    (r, id)
}

#[test] fn unknown_node_arrives_pending_even_if_accepted_there() {
    let (mut l, _) = reg(A, "a", &[], Admission::Accepted);
    let (o, id) = reg(B, "b", &["http://b:1"], Admission::Accepted);
    let rep = l.merge(&o);
    assert_eq!(rep.added_pending, 1);
    assert_eq!(l.entries[&id].admission, Admission::Pending);
    assert_eq!(l.entries[&id].urls, vec!["http://b:1".to_string()]);
    assert!(!l.is_accepted(B));
}
#[test] fn local_decision_and_label_survive() {
    let (mut l, id) = reg(A, "mine", &[], Admission::Ignored);
    let (o, _) = reg(A, "theirs", &[], Admission::Accepted);
    l.merge(&o);
    assert_eq!(l.entries[&id].admission, Admission::Ignored);
    assert_eq!(l.entries[&id].label, "mine");
}
#[test] fn urls_are_a_sorted_union() {
    let (mut l, id) = reg(A, "a", &["http://z:1", "http://m:1"], Admission::Accepted);
    let (o, _) = reg(A, "a", &["http://m:1", "http://a:1"], Admission::Accepted);
    let rep = l.merge(&o);
    assert_eq!(rep.updated, 1);
    assert_eq!(l.entries[&id].urls, vec!["http://a:1", "http://m:1", "http://z:1"]);
}
#[test] fn sponsor_is_taken_only_when_missing() {
    let s = |n: &str| SponsorSignature { sponsor_node_id: n.into(), signature: "ab".into() };
    let (mut l, id) = reg(A, "a", &[], Admission::Accepted);
    let (mut o, _) = reg(A, "a", &[], Admission::Accepted);
    o.entries.get_mut(&id).unwrap().sponsor = Some(s("first"));
    assert_eq!(l.merge(&o).updated, 1);
    assert_eq!(l.entries[&id].sponsor.as_ref().unwrap().sponsor_node_id, "first");
    o.entries.get_mut(&id).unwrap().sponsor = Some(s("second"));
    assert_eq!(l.merge(&o).updated, 0);
    assert_eq!(l.entries[&id].sponsor.as_ref().unwrap().sponsor_node_id, "first");
}
#[test] fn nothing_new_counts_nothing() {
    let (mut l, _) = reg(A, "a", &["http://a:1"], Admission::Accepted);
    let (o, _) = reg(A, "a", &["http://a:1"], Admission::Accepted);
    assert_eq!(l.merge(&o), MergeReport::default());
}
#[test] fn forged_record_changes_nothing() {
    let (mut l, id) = reg(A, "a", &["http://a:1"], Admission::Accepted);
    let before = l.clone();
    let (mut o, _) = reg(A, "a", &["http://evil:1"], Admission::Accepted);
    o.entries.get_mut(&id).unwrap().pubkey = B.to_string();
    let rep = l.merge(&o);
    assert_eq!(rep.conflicts, vec![id]);
    assert_eq!(l, before);
}
#[test] fn record_filed_under_the_wrong_id_is_a_conflict() {
    let (mut l, _) = reg(A, "a", &[], Admission::Accepted);
    let (mut o, id) = reg(B, "b", &[], Admission::Accepted);
    let e = o.entries.remove(&id).unwrap();
    o.entries.insert("deadbeef".into(), e);
    let rep = l.merge(&o);
    assert_eq!(rep.conflicts, vec!["deadbeef".to_string()]);
    assert_eq!(l.entries.len(), 1);
}
