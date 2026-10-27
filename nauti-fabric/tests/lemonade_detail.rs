//! NF1: the Lemonade capability detail map reports what is LOADED, the real
//! compute backend and the AMD GPU list — never downloaded models as serving
//! and never recipe names as the backend. Values below are from a live
//! Lemonade 2026.41.1 server on t440 (2026-10-25).

use nauti_fabric::adapters::{lemonade_detail, LemonadeLoaded, LemonadeModel, LemonadeReport};

fn downloaded(id: &str, size: f64) -> LemonadeModel {
    LemonadeModel { id: id.into(), backend: "llamacpp".into(), downloaded: true, size_gb: Some(size) }
}

fn loaded(id: &str) -> LemonadeLoaded {
    LemonadeLoaded { id: id.into(), recipe: "llamacpp".into(), device: "gpu".into(), status: "ready".into() }
}

fn live_report() -> LemonadeReport {
    LemonadeReport {
        reachable: true,
        version: Some("2026.41.1".into()),
        models: vec![
            downloaded("Qwen3-0.6B-GGUF", 0.36),
            downloaded("Qwen3-VL-4B-Instruct-GGUF", 3.10),
            downloaded("nomic-embed-text-v1-GGUF", 0.07),
        ],
        amd_gpus: vec![
            "Radeon RX 7900 XTX (0000:03:00.0)".into(),
            "Radeon RX 7900 XTX (0000:83:00.0)".into(),
        ],
        loaded: vec![loaded("Qwen3-VL-4B-Instruct-GGUF"), loaded("nomic-embed-text-v1-GGUF")],
        backends: vec!["vulkan".into()],
    }
}

#[test]
fn lemonade_detail_live_report() {
    let d = lemonade_detail(&live_report());
    assert_eq!(d["lemonade.reachable"], "true");
    assert_eq!(d["lemonade.version"], "2026.41.1");
    assert_eq!(d["lemonade.models"], "Qwen3-VL-4B-Instruct-GGUF,nomic-embed-text-v1-GGUF");
    assert_eq!(d["lemonade.downloaded"], "Qwen3-0.6B-GGUF,Qwen3-VL-4B-Instruct-GGUF,nomic-embed-text-v1-GGUF");
    assert_eq!(d["lemonade.backend"], "vulkan");
    assert_eq!(d["lemonade.amd_gpus"], "Radeon RX 7900 XTX (0000:03:00.0); Radeon RX 7900 XTX (0000:83:00.0)");
}

#[test]
fn lemonade_detail_idle_server_is_none_loaded_and_has_no_recipe_backend() {
    let mut r = live_report();
    r.loaded.clear();
    r.backends.clear();
    let d = lemonade_detail(&r);
    assert_eq!(d["lemonade.models"], "none-loaded");
    // Downloaded models still listed, but separately.
    assert!(d["lemonade.downloaded"].contains("Qwen3-0.6B-GGUF"));
    // Recipe names ("llamacpp") are never reported as the backend.
    assert!(!d.contains_key("lemonade.backend"));
    assert!(!d.values().any(|v| v == "llamacpp"));
}

#[test]
fn lemonade_detail_omits_empty_lists_and_unknown_version() {
    let r = LemonadeReport { reachable: false, ..Default::default() };
    let d = lemonade_detail(&r);
    assert_eq!(d["lemonade.reachable"], "false");
    assert_eq!(d["lemonade.models"], "none-loaded");
    assert!(!d.contains_key("lemonade.version"));
    assert!(!d.contains_key("lemonade.downloaded"));
    assert!(!d.contains_key("lemonade.backend"));
    assert!(!d.contains_key("lemonade.amd_gpus"));
}

#[test]
fn lemonade_detail_backends_joined_in_order() {
    let mut r = live_report();
    r.backends = vec!["vulkan".into(), "cpu".into()];
    assert_eq!(lemonade_detail(&r)["lemonade.backend"], "vulkan,cpu");
}
