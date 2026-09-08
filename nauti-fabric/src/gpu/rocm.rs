//! AMD ROCm enrichment — the AMD counterpart to the NVML `nvidia` feature.
//!
//! **`all-smi` is still the authority.** This module is *enrichment only*:
//! it never gates discovery and never creates resources. It attaches
//! AMD-specific telemetry (marketing name, UUID, temperature, utilization,
//! VRAM usage) onto `GpuDevice` entries that the all-smi DRM walk already
//! found, exactly the way `nvml-wrapper` does for NVIDIA.
//!
//! Two sources, tried in order, keyed by PCI BDF:
//!
//! 1. **`rocm-smi` CLI** (if the ROCm userspace is installed):
//!    best data — real product names ("Radeon Pro WX 5100"), per-card
//!    VRAM, utilization, temperature.
//! 2. **amdgpu sysfs + hwmon** (always present when the kernel driver
//!    binds, no ROCm install needed): `mem_info_vram_total/used`,
//!    `gpu_busy_percent`, hwmon `temp1_input`, and — on newer kernels —
//!    `product_name` / `unique_id`.
//!
//! The host operator does **not** need ROCm installed; the sysfs path works
//! everywhere amdgpu does. When ROCm *is* installed, the adapter prefers it
//! for the fields sysfs cannot provide. The `rocm` feature flag only gates
//! compilation — it adds zero dependencies.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use super::discover::{GpuDevice, GpuVendor};

/// One AMD card's enriched telemetry, keyed by PCI BDF.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AmdTelemetry {
    pub product_name: Option<String>,
    pub unique_id: Option<String>,
    pub vram_total_bytes: Option<u64>,
    pub vram_used_bytes: Option<u64>,
    pub utilization_pct: Option<u32>,
    pub temperature_c: Option<u32>,
    /// Which source produced this: "rocm-smi", "rocm-sysfs", or a mix.
    pub source: String,
}

/// Collect AMD telemetry for every card on the system.
///
/// `sysfs_base` is injected for tests (production passes `/sys`); `drm_dir`
/// likewise (production `/sys/class/drm`). Returns a BDF → telemetry map.
pub fn collect(sysfs_base: &Path, drm_dir: &Path) -> BTreeMap<String, AmdTelemetry> {
    let mut map = sysfs_enrichment(sysfs_base, drm_dir);
    merge_rocm_smi(&mut map);
    map.retain(|_, t| {
        t.product_name.is_some()
            || t.unique_id.is_some()
            || t.vram_total_bytes.is_some()
            || t.vram_used_bytes.is_some()
            || t.utilization_pct.is_some()
            || t.temperature_c.is_some()
    });
    map
}

/// Apply the map onto discovered devices (AMD cards only, never the
/// authority — display-only BMC VGA is skipped too).
pub fn enrich(devices: &mut [GpuDevice], telemetry: &BTreeMap<String, AmdTelemetry>) {
    for dev in devices.iter_mut() {
        if !matches!(dev.vendor, GpuVendor::Amd) || dev.display_only {
            continue;
        }
        if let Some(t) = telemetry.get(&dev.pci_bdf) {
            if let Some(name) = &t.product_name {
                if !name.is_empty() {
                    dev.device_name = name.clone();
                }
            }
            if t.unique_id.is_some() {
                dev.uuid = t.unique_id.clone();
            }
            if let Some(v) = t.vram_total_bytes {
                if v > 0 {
                    dev.vram_total_bytes = v;
                }
            }
            if let Some(v) = t.vram_used_bytes {
                dev.vram_used_bytes = v;
            }
            if t.utilization_pct.is_some() {
                dev.utilization_pct = t.utilization_pct;
            }
            if t.temperature_c.is_some() {
                dev.temperature_c = t.temperature_c;
            }
        }
    }
}

/// Source 2: pure sysfs, no ROCm userspace required. Works wherever the
/// amdgpu kernel driver has bound the card.
pub fn sysfs_enrichment(sysfs_base: &Path, drm_dir: &Path) -> BTreeMap<String, AmdTelemetry> {
    let mut map: BTreeMap<String, AmdTelemetry> = BTreeMap::new();
    let entries = match std::fs::read_dir(drm_dir) {
        Ok(e) => e,
        Err(_) => return map,
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let dev = drm_dir.join(format!("{}/device", name));
        let bdf = std::fs::read_link(&dev).ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_default();
        if bdf.is_empty() {
            continue;
        }
        let vendor = std::fs::read_to_string(dev.join("vendor")).ok()
            .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
            .unwrap_or(0);
        if vendor != 0x1002 && vendor != 0x1022 {
            continue;
        }

        let mut t = AmdTelemetry { source: "rocm-sysfs".into(), ..Default::default() };
        let product = std::fs::read_to_string(dev.join("product_name")).ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if let Some(p) = product {
            t.product_name = Some(p);
        }
        let uid = std::fs::read_to_string(dev.join("unique_id")).ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if let Some(u) = uid {
            t.unique_id = Some(u);
        }
        if let Some(v) = read_u64(&dev.join("mem_info_vram_total")) {
            t.vram_total_bytes = Some(v);
        }
        if let Some(v) = read_u64(&dev.join("mem_info_vram_used")) {
            t.vram_used_bytes = Some(v);
        }
        // hwmon: temp1_input is millidegrees C.
        if let Ok(hms) = std::fs::read_dir(dev.join("hwmon")) {
            for hm in hms.flatten() {
                if let Some(milli) = read_u64(&hm.path().join("temp1_input")) {
                    t.temperature_c = Some((milli / 1000) as u32);
                }
                break;
            }
        }
        map.insert(bdf, t);
    }
    let _ = sysfs_base; // reserved: future non-DRM sysfs sources
    map
}

/// Source 1: `rocm-smi` CLI, when the ROCm userspace is installed. Overrides
/// sysfs values per-card (it reads the same kernel interfaces but adds
/// product names and utilization that sysfs lacks on older kernels).
pub fn merge_rocm_smi(map: &mut BTreeMap<String, AmdTelemetry>) {
    // rocm-smi reports "card0"-style ids; map those to BDFs via DRM first.
    let card_to_bdf = card_index_to_bdf();
    let output = match Command::new("rocm-smi")
        .args(["--showproductname", "--showmeminfo", "vram",
               "--showuse", "--showtemp", "--json"])
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return,
    };
    let parsed: serde_json::Value = match serde_json::from_str(&output) {
        Ok(v) => v,
        Err(_) => return,
    };

    // rocm-smi nests per-card tables; structure varies across ROCm releases,
    // so walk every top-level table and pull out known keys liberally.
    let Some(tables) = parsed.as_object() else { return };
    for (key, table) in tables {
        let bdf: Option<String> = if let Some(i) = key.strip_prefix("card") {
            i.parse::<u32>().ok().and_then(|i| card_to_bdf.get(&i).cloned())
        } else {
            Some(key.clone())
        };
        let Some(bdf) = bdf else { continue };
        let Some(obj) = table.as_object() else { continue };

        let entry = map.entry(bdf).or_insert_with(|| {
            AmdTelemetry { source: "rocm-smi".into(), ..Default::default() }
        });
        if entry.source == "rocm-sysfs" {
            entry.source = "rocm-smi".into();
        }
        for (k, v) in obj {
            let kl = k.to_lowercase();
            if (kl.contains("product name") || kl.contains("vbios"))
                && entry.product_name.is_none()
            {
                if let Some(s) = v.as_str() {
                    if !s.is_empty() { entry.product_name = Some(s.to_string()); }
                }
            }
            if (kl.contains("unique_id") || kl.contains("unique id"))
                && entry.unique_id.is_none()
            {
                if let Some(s) = v.as_str() {
                    if !s.is_empty() { entry.unique_id = Some(s.to_string()); }
                }
            }
            if kl.contains("total") && kl.contains("vram") && entry.vram_total_bytes.is_none() {
                if let Some(bytes) = v.as_str().and_then(parse_human_bytes) {
                    entry.vram_total_bytes = Some(bytes);
                }
            }
            if kl.contains("used") && kl.contains("vram") && entry.vram_used_bytes.is_none() {
                if let Some(bytes) = v.as_str().and_then(parse_human_bytes) {
                    entry.vram_used_bytes = Some(bytes);
                }
            }
            if (kl.contains("gpu use") || kl.contains("gpu utilization"))
                && entry.utilization_pct.is_none()
            {
                if let Some(pct) = v.as_str().and_then(parse_pct) {
                    entry.utilization_pct = Some(pct);
                }
            }
            if kl.contains("temperature") && entry.temperature_c.is_none() {
                if let Some(c) = v.as_str().and_then(parse_pct) {
                    entry.temperature_c = Some(c);
                }
            }
        }
    }
}

/// Map DRM card index → PCI BDF by reading the device symlink.
fn card_index_to_bdf() -> BTreeMap<u32, String> {
    let mut map = BTreeMap::new();
    if let Ok(entries) = std::fs::read_dir("/sys/class/drm") {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.starts_with("card") || name.contains('-') { continue; }
            if let Ok(idx) = name["card".len()..].parse::<u32>() {
                if let Ok(link) = std::fs::read_link(e.path().join("device")) {
                    if let Some(bdf) = link.file_name() {
                        map.insert(idx, bdf.to_string_lossy().to_string());
                    }
                }
            }
        }
    }
    map
}

/// "16368.0 MB" → bytes. rocm-smi reports MiB with that spelling.
fn parse_human_bytes(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, unit) = s.split_once(char::is_whitespace)?;
    let n: f64 = num.trim().parse().ok()?;
    let mult: u64 = if unit.to_uppercase().contains('G') {
        1024 * 1024 * 1024
    } else {
        1024 * 1024 // MiB default; rocm-smi vram tables are MiB
    };
    Some((n * mult as f64) as u64)
}

/// "12.0" or "12%" → 12. Accepts floats (rocm-smi reports "44.0").
fn parse_pct(s: &str) -> Option<u32> {
    let s = s.trim().trim_end_matches('%').trim();
    s.parse::<f64>().ok().map(|f| f as u32)
}

fn read_u64(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path).ok().and_then(|s| s.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Build a fake amdgpu sysfs tree in a temp dir and assert the
    /// enrichment reads it correctly — no hardware required.
    #[test]
    fn sysfs_enrichment_reads_amdgpu_tree() {
        let base = std::env::temp_dir().join(format!("nauti-rocm-test-{}", std::process::id()));
        fs::remove_dir_all(&base).ok(); // clean any stale run
        let dev = base.join("class/drm/card1/device");
        fs::create_dir_all(base.join("class/drm/card1")).unwrap();
        let bdf_dir = base.join("devices/0000:03:00.0");
        let hwmon = bdf_dir.join("hwmon/hwmon0");
        fs::create_dir_all(&hwmon).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&bdf_dir, &dev).unwrap();

        fs::write(dev.join("vendor"), "0x1002").unwrap();
        fs::write(dev.join("product_name"), "Radeon Pro WX 5100\n").unwrap();
        fs::write(dev.join("unique_id"), "876543210987654\n").unwrap();
        fs::write(dev.join("mem_info_vram_total"), "8589934592\n").unwrap();
        fs::write(dev.join("mem_info_vram_used"), "640000000\n").unwrap();
        fs::write(hwmon.join("temp1_input"), "45500\n").unwrap();

        let map = sysfs_enrichment(&base, &base.join("class/drm"));
        let t = map.get("0000:03:00.0").expect("AMD card enriched");
        assert_eq!(t.product_name.as_deref(), Some("Radeon Pro WX 5100"));
        assert_eq!(t.unique_id.as_deref(), Some("876543210987654"));
        assert_eq!(t.vram_total_bytes, Some(8589934592));
        assert_eq!(t.vram_used_bytes, Some(640000000));
        assert_eq!(t.temperature_c, Some(45));
        assert_eq!(t.source, "rocm-sysfs");

        // Non-AMD vendor must be skipped.
        fs::write(dev.join("vendor"), "0x10de").unwrap();
        let map2 = sysfs_enrichment(&base, &base.join("class/drm"));
        assert!(!map2.contains_key("0000:03:00.0"), "NVIDIA card must not be enriched by the AMD path");

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn human_bytes_parsing() {
        assert_eq!(parse_human_bytes("16368.0 MB"), Some(16368 * 1024 * 1024));
        assert_eq!(parse_human_bytes("8.0 GB"), Some(8 * 1024 * 1024 * 1024));
        assert_eq!(parse_human_bytes("garbage"), None);
    }

    #[test]
    fn pct_parsing() {
        assert_eq!(parse_pct("12"), Some(12));
        assert_eq!(parse_pct("44.0"), Some(44));
        assert_eq!(parse_pct("0%"), Some(0));
        assert_eq!(parse_pct(""), None);
    }

    /// Live-host test: enrichment must be infallible even on hosts with
    /// zero AMD cards and no ROCm installed.
    #[test]
    fn collect_is_infallible() {
        let _ = collect(Path::new("/sys"), Path::new("/sys/class/drm"));
    }
}
