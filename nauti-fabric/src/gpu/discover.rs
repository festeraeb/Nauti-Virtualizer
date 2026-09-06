//! All-brand GPU discovery via Linux DRM sysfs (`/sys/class/drm`).
//!
//! **`all-smi` is the authority.** This adapter walks every DRM node the
//! kernel exposes and identifies the GPU by its PCI vendor/device id — no
//! vendor-specific management library is *required*. NVIDIA, AMD, Intel, and
//! anything else the kernel drives through `/sys/class/drm/card*` always
//! show up here.
//!
//! `nvidia-smi` (via the `nvml-wrapper` Rust library) is **optional
//! enrichment only** — it is compiled behind the `nvidia` feature and adds
//! NVIDIA-specific telemetry (UUID, real model name, utilization, temp) that
//! DRM does not expose. It is NEVER the source of truth, and AMD / Intel /
//! any-brand cards are discovered and reported identically without it.
//!
//! VRAM for AMD comes from the `amdgpu` sysfs file `mem_info_vram_total`
//! (present whenever amdgpu binds the card); NVIDIA cards get it from NVML
//! when available, else they report 0 MB and rely on the enumeration. The
//! result is one entry per physical GPU with the best data available for its
//! brand — no duplicates, no static maps, no scripts.
//!
//! Identity is the PCI BDF (`0000:03:00.0`): stable across reboots and unique
//! per slot, so a hot-swap is detected as "old card gone, new card appeared"
//! rather than "the same card changed."

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Resource;

/// A single physical GPU, identified by PCI BDF and enriched with whatever
/// brand-specific data is available.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GpuDevice {
    /// Stable identity: PCI BDF like \0470000:03:00.0\047. Survives reboots.
    pub stable_id: String,
    pub vendor: GpuVendor,
    pub device_name: String,
    pub vram_total_bytes: u64,
    pub vram_used_bytes: u64,
    pub driver: String,
    pub drm_card: Option<u32>,
    pub pci_bdf: String,
    pub uuid: Option<String>,
    pub nvml_index: Option<u32>,
    pub temperature_c: Option<u32>,
    pub utilization_pct: Option<u32>,
    /// True for BMC VGA controllers (ASPEED ast, Matrox mgag200).
    pub display_only: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Unknown(String),
}

impl GpuVendor {
    fn from_pci_id(id: u32) -> Self {
        match id {
            0x10de => GpuVendor::Nvidia,
            0x1002 | 0x1022 => GpuVendor::Amd,
            0x8086 => GpuVendor::Intel,
            other => GpuVendor::Unknown(format!("0x{other:04x}")),
        }
    }

    pub fn label(&self) -> String {
        match self {
            GpuVendor::Nvidia => "NVIDIA".into(),
            GpuVendor::Amd => "AMD".into(),
            GpuVendor::Intel => "Intel".into(),
            GpuVendor::Unknown(s) => s.clone(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GpuDiscoveryError {
    #[error("cannot read /sys/class/drm: {0}")]
    Sysfs(String),
}

/// The result of a live discovery pass. Always-compiled; works on any Linux
/// host with DRM nodes, no optional features required.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct GpuDiscoveryResult {
    pub devices: Vec<GpuDevice>,
}

impl GpuDiscoveryResult {
    /// Walk /sys/class/drm once and return every GPU found.
    pub fn discover() -> Result<Self, GpuDiscoveryError> {
        let mut result = Self::default();
        let drm_dir = Path::new("/sys/class/drm");
        if !drm_dir.exists() {
            return Ok(result);
        }

        let mut entries: Vec<(u32, GpuDevice)> = Vec::new();
        let read_dir = fs::read_dir(drm_dir)
            .map_err(|e| GpuDiscoveryError::Sysfs(e.to_string()))?;
        for entry in read_dir.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("card") || name.contains('-') {
                continue;
            }
            let card_idx: u32 = match name["card".len()..].parse() {
                Ok(i) => i,
                Err(_) => continue,
            };
            if let Some(dev) = Self::read_drm_card(card_idx) {
                entries.push((card_idx, dev));
            }
        }

        let nvml_by_bdf = Self::nvml_by_bdf();
        let mut devices: Vec<GpuDevice> = Vec::with_capacity(entries.len());
        for (_card_idx, mut dev) in entries {
            if matches!(dev.vendor, GpuVendor::Nvidia) {
                if let Some(nvml) = nvml_by_bdf.get(&dev.pci_bdf) {
                    dev.device_name = nvml.0.clone();
                    dev.uuid = Some(nvml.1.clone());
                    dev.nvml_index = Some(nvml.2);
                    dev.vram_total_bytes = nvml.3;
                    dev.vram_used_bytes = nvml.4;
                    dev.utilization_pct = nvml.5;
                    dev.temperature_c = nvml.6;
                }
            }
            devices.push(dev);
        }

        devices.sort_by(|a, b| {
            fn rank(d: &GpuDevice) -> u8 {
                if d.display_only { return 3; }
                match d.vendor {
                    GpuVendor::Nvidia => 0,
                    GpuVendor::Amd => 1,
                    _ => 2,
                }
            }
            rank(a).cmp(&rank(b)).then_with(|| a.pci_bdf.cmp(&b.pci_bdf))
        });

        result.devices = devices;
        Ok(result)
    }

    fn read_drm_card(card_idx: u32) -> Option<GpuDevice> {
        let dev_dir = format!("/sys/class/drm/card{card_idx}/device");
        let dev_path = Path::new(&dev_dir);
        if !dev_path.exists() { return None; }

        let vendor_id = read_hex(dev_path.join("vendor")).unwrap_or(0);
        if vendor_id == 0 { return None; }
        let vendor = GpuVendor::from_pci_id(vendor_id);

        let uevent = fs::read_to_string(dev_path.join("uevent")).unwrap_or_default();
        let driver = uevent.lines()
            .find(|l| l.starts_with("DRIVER="))
            .map(|l| l["DRIVER=".len()..].to_string())
            .unwrap_or_default();

        let pci_bdf = fs::read_link(dev_path).ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_default();

        let vram_total_bytes = read_u64(dev_path.join("mem_info_vram_total")).unwrap_or(0);
        let vram_used_bytes = read_u64(dev_path.join("mem_info_vram_used")).unwrap_or(0);
        let busy_pct = read_u32(dev_path.join("gpu_busy_percent")).unwrap_or(0);
        let display_only = matches!(driver.as_str(), "ast" | "mgag200");
        let device_name = format!("{} GPU ({})", vendor.label(), pci_bdf);

        Some(GpuDevice {
            stable_id: pci_bdf.clone(),
            vendor, device_name, vram_total_bytes, vram_used_bytes,
            driver, drm_card: Some(card_idx), pci_bdf,
            uuid: None, nvml_index: None, temperature_c: None,
            utilization_pct: if busy_pct > 0 { Some(busy_pct) } else { None },
            display_only,
        })
    }

    #[cfg(feature = "nvidia")]
    fn nvml_by_bdf() -> BTreeMap<String, (String, String, u32, u64, u64, Option<u32>, Option<u32>)> {
        use nvml_wrapper::enum_wrappers::device::TemperatureSensor;
        let mut map = BTreeMap::new();
        let nvml = match nvml_wrapper::Nvml::init() { Ok(n) => n, Err(_) => return map };
        let count = match nvml.device_count() { Ok(c) => c, Err(_) => return map };
        for idx in 0..count {
            let device = match nvml.device_by_index(idx) { Ok(d) => d, Err(_) => continue };
            let name = device.name().unwrap_or_default();
            let uuid = device.uuid().unwrap_or_default();
            let memory = device.memory_info().map(|m| (m.total, m.used)).unwrap_or((0, 0));
            let util = device.utilization_rates().ok().map(|u| u.gpu);
            let temp = device.temperature(TemperatureSensor::Gpu).ok();
            let bdf = device.pci_info().ok().map(|p| normalize_bdf(&p.bus_id)).unwrap_or_default();
            if !bdf.is_empty() {
                map.insert(bdf, (name, uuid, idx, memory.0, memory.1, util, temp));
            }
        }
        map
    }

    #[cfg(not(feature = "nvidia"))]
    fn nvml_by_bdf() -> BTreeMap<String, (String, String, u32, u64, u64, Option<u32>, Option<u32>)> {
        BTreeMap::new()
    }

    pub fn grouped(&self) -> BTreeMap<String, Vec<&GpuDevice>> {
        let mut groups: BTreeMap<String, Vec<&GpuDevice>> = BTreeMap::new();
        for dev in &self.devices {
            let key = if dev.display_only {
                "__display_only__".to_string()
            } else {
                dev.vendor.label()
            };
            groups.entry(key).or_default().push(dev);
        }
        groups
    }

    pub fn as_resources(&self, node: impl Into<String>) -> Vec<Resource> {
        let node = node.into();
        self.devices.iter().filter(|d| !d.display_only).map(|gpu| {
            let mut attrs = BTreeMap::from([
                ("gpu.stable_id".into(), gpu.stable_id.clone()),
                ("gpu.vendor".into(), gpu.vendor.label()),
                ("gpu.device_name".into(), gpu.device_name.clone()),
                ("gpu.driver".into(), gpu.driver.clone()),
                ("gpu.pci_bdf".into(), gpu.pci_bdf.clone()),
                ("gpu.vram_total_bytes".into(), gpu.vram_total_bytes.to_string()),
                ("gpu.vram_used_bytes".into(), gpu.vram_used_bytes.to_string()),
            ]);
            if let Some(uuid) = &gpu.uuid { attrs.insert("gpu.uuid".into(), uuid.clone()); }
            if let Some(idx) = gpu.nvml_index { attrs.insert("gpu.nvml_index".into(), idx.to_string()); }
            Resource {
                id: format!("local.gpu.{}", gpu.stable_id.replace([':', '.'], "_")),
                kind: crate::ResourceKind::Gpu,
                capacity: gpu.vram_total_bytes,
                unit: "bytes".into(),
                node: node.clone(),
                state: crate::ResourceState::Available,
                exclusive: true,
                attributes: attrs,
            }
        }).collect()
    }
}

fn normalize_bdf(raw: &str) -> String {
    let trimmed = raw.trim().to_lowercase();
    if trimmed.is_empty() { return String::new(); }
    let parts: Vec<&str> = trimmed.split(':').collect();
    if parts.len() == 3 {
        let domain = if parts[0].len() > 4 { &parts[0][parts[0].len()-4..] } else { parts[0] };
        format!("{}:{}:{}", domain, parts[1], parts[2])
    } else {
        trimmed
    }
}

fn read_hex(path: impl AsRef<Path>) -> Option<u32> {
    fs::read_to_string(path).ok()
        .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
}

fn read_u64(path: impl AsRef<Path>) -> Option<u64> {
    fs::read_to_string(path).ok().and_then(|s| s.trim().parse().ok())
}

fn read_u32(path: impl AsRef<Path>) -> Option<u32> {
    fs::read_to_string(path).ok().and_then(|s| s.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_is_infallible() {
        let result = GpuDiscoveryResult::discover();
        assert!(result.is_ok(), "discovery must never panic: {result:?}");
    }

    #[cfg(feature = "nvidia")]
    #[test]
    fn discover_finds_nvidia_and_enriches_name() {
        let result = GpuDiscoveryResult::discover().expect("discover infallible");
        assert!(!result.devices.is_empty(), "expected at least one GPU on this host");
        let nvidia: Vec<_> = result.devices.iter()
            .filter(|d| matches!(d.vendor, GpuVendor::Nvidia)).collect();
        assert!(!nvidia.is_empty(), "expected NVIDIA cards");
        for g in &nvidia {
            assert!(!g.device_name.is_empty()
                && !g.device_name.starts_with("NVIDIA GPU (0000:00:00"),
                "NVML should provide a real name, got {}", g.device_name);
            assert!(g.uuid.is_some(), "NVML should provide a UUID");
            assert!(g.vram_total_bytes > 0, "NVML should report VRAM");
        }
    }

    #[test]
    fn vendor_from_pci_id() {
        assert!(matches!(GpuVendor::from_pci_id(0x10de), GpuVendor::Nvidia));
        assert!(matches!(GpuVendor::from_pci_id(0x1002), GpuVendor::Amd));
        assert!(matches!(GpuVendor::from_pci_id(0x8086), GpuVendor::Intel));
        assert!(matches!(GpuVendor::from_pci_id(0x1234), GpuVendor::Unknown(_)));
    }

    #[test]
    fn normalize_bdf_handles_nvml_padding() {
        assert_eq!(normalize_bdf("00000000:03:00.0"), "0000:03:00.0");
        assert_eq!(normalize_bdf("0000:03:00.0"), "0000:03:00.0");
        assert_eq!(normalize_bdf(""), "");
    }

    #[test]
    fn as_resources_skips_display_only() {
        let result = GpuDiscoveryResult { devices: vec![
            GpuDevice {
                stable_id: "0000:03:00.0".into(), vendor: GpuVendor::Nvidia,
                device_name: "RTX 2060 SUPER".into(), vram_total_bytes: 8 << 30,
                vram_used_bytes: 0, driver: "nvidia".into(), drm_card: Some(0),
                pci_bdf: "0000:03:00.0".into(), uuid: Some("GPU-abc".into()),
                nvml_index: Some(0), temperature_c: None, utilization_pct: None,
                display_only: false,
            },
            GpuDevice {
                stable_id: "0000:01:00.0".into(),
                vendor: GpuVendor::Unknown("0x1a03".into()),
                device_name: "ASPEED BMC".into(), vram_total_bytes: 0,
                vram_used_bytes: 0, driver: "ast".into(), drm_card: Some(1),
                pci_bdf: "0000:01:00.0".into(), uuid: None, nvml_index: None,
                temperature_c: None, utilization_pct: None, display_only: true,
            },
        ]};
        let resources = result.as_resources("test-node");
        assert_eq!(resources.len(), 1, "display-only BMC must be filtered out");
        assert_eq!(resources[0].attributes["gpu.device_name"], "RTX 2060 SUPER");
    }
}
