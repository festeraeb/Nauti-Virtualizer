//! Seeded gate for Nexus Resiliency §9 T7: after a successful bulk connect, one info line names the file, its byte
//! length and the chosen path (direct or relay). Written by the controller before the forge task; the forge must not
//! edit this file. The line's text is a pure function so it is testable without a network; the wiring is checked in
//! the source of `QuicChunkSource::connect_auto`.
use nauti_fabric::data_quic::bulk_path_line;

#[test]
fn the_line_names_file_bytes_and_direct() {
    let line = bulk_path_line("m.gguf", 100_000, false, "*direct Ip(10.44.0.14:4433) rtt 0.21 ms");
    assert_eq!(line, "bulk m.gguf: 100000 bytes over direct (*direct Ip(10.44.0.14:4433) rtt 0.21 ms)");
}

#[test]
fn the_line_names_relay_when_relayed() {
    let line = bulk_path_line("w/q4.gguf", 7, true, "*relay Relay(https://r.example/) rtt 40.00 ms");
    assert_eq!(line, "bulk w/q4.gguf: 7 bytes over relay (*relay Relay(https://r.example/) rtt 40.00 ms)");
}

/// The body of `pub fn connect_auto` in data_quic.rs, up to the next `pub fn`.
fn connect_auto_body() -> &'static str {
    let src = include_str!("../src/data_quic.rs");
    let start = src.find("pub fn connect_auto(").expect("QuicChunkSource::connect_auto exists (T6)");
    let rest = &src[start..];
    let end = rest[1..].find("pub fn ").map(|i| i + 1).unwrap_or(rest.len());
    &rest[..end]
}

#[test]
fn connect_auto_logs_the_line_once() {
    let body = connect_auto_body();
    assert_eq!(body.matches("info!(").count(), 1, "exactly one info! line in connect_auto:\n{body}");
    for needle in ["bulk_path_line(", "is_relayed()", ".path()", "stat("] {
        assert!(body.contains(needle), "connect_auto should use {needle}:\n{body}");
    }
}
