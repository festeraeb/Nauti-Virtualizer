//! Optional NVIDIA GPU capture via `nvml-wrapper` (a safe wrapper around NVIDIA's Management
//! Library, `libnvidia-ml.so`).
//!
//! This module is compiled only when the `nvidia` feature is enabled, since it links against
//! the native NVML shared library (installed alongside any recent NVIDIA driver). It is kept
//! separate from `HostInventory::discover` (which has no native dependencies) so that the
//! default build stays dependency-light; callers opt in explicitly by calling
//! [`GpuTopology::discover`]. On hosts with no NVIDIA driver/GPU present, `discover` returns a
//! `GpuError::Load` rather than panicking, so callers can treat "no NVIDIA stack available" as
//! an ordinary, recoverable condition.

use std::collections::BTreeMap;

use nvml_wrapper::Nvml;
use serde::{Deserialize, Serialize};

use crate::{Resource, ResourceKind, ResourceState};

/// A single NVIDIA GPU discovered via NVML.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GpuDeviceReport {
    pub index: u32,
    pub name: String,
    pub uuid: String,
    pub total_memory_bytes: u64,
    pub free_memory_bytes: u64,
    pub pci_bus_id: String,
}

/// Errors that can occur while probing NVIDIA GPUs.
#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    #[error("failed to initialize NVML (no NVIDIA driver/GPU present?): {0}")]
    Load(String),
    #[error("failed to query NVML device {index}: {reason}")]
    Device { index: u32, reason: String },
}

/// Thin wrapper around an NVML session, exposing just the GPU facts that are useful for
/// resource-adapter capability reporting.
pub struct GpuTopology {
    devices: Vec<GpuDeviceReport>,
}

impl GpuTopology {
    /// Enumerate NVIDIA GPUs visible via NVML. Requires an NVIDIA driver (and thus
    /// `libnvidia-ml.so`) to be present on the system; returns `GpuError::Load` (not a panic)
    /// on hosts without one, so callers can treat "no NVIDIA GPU here" as an ordinary,
    /// recoverable condition rather than a hard failure.
    pub fn discover() -> Result<Self, GpuError> {
        let nvml = Nvml::init().map_err(|e| GpuError::Load(e.to_string()))?;
        let count = nvml
            .device_count()
            .map_err(|e| GpuError::Load(e.to_string()))?;

        let mut devices = Vec::with_capacity(count as usize);
        for index in 0..count {
            let device = nvml
                .device_by_index(index)
                .map_err(|e| GpuError::Device { index, reason: e.to_string() })?;
            let name = device
                .name()
                .map_err(|e| GpuError::Device { index, reason: e.to_string() })?;
            let uuid = device
                .uuid()
                .map_err(|e| GpuError::Device { index, reason: e.to_string() })?;
            let memory = device
                .memory_info()
                .map_err(|e| GpuError::Device { index, reason: e.to_string() })?;
            let pci_info = device
                .pci_info()
                .map_err(|e| GpuError::Device { index, reason: e.to_string() })?;

            devices.push(GpuDeviceReport {
                index,
                name,
                uuid,
                total_memory_bytes: memory.total,
                free_memory_bytes: memory.free,
                pci_bus_id: pci_info.bus_id,
            });
        }

        Ok(Self { devices })
    }

    pub fn devices(&self) -> &[GpuDeviceReport] {
        &self.devices
    }

    /// Convert discovered GPUs into `Resource` entries (kind `Gpu`, one per device), tagged
    /// with device attributes so callers can pick a specific GPU by UUID/PCI bus id.
    pub fn as_resources(&self, node: impl Into<String>) -> Vec<Resource> {
        let node = node.into();
        self.devices
            .iter()
            .map(|gpu| Resource {
                id: format!("local.gpu.{}", gpu.index),
                kind: ResourceKind::Gpu,
                capacity: gpu.total_memory_bytes,
                unit: "bytes".into(),
                node: node.clone(),
                state: ResourceState::Available,
                exclusive: true,
                attributes: BTreeMap::from([
                    ("gpu.name".into(), gpu.name.clone()),
                    ("gpu.uuid".into(), gpu.uuid.clone()),
                    ("gpu.pci_bus_id".into(), gpu.pci_bus_id.clone()),
                    ("gpu.free_memory_bytes".into(), gpu.free_memory_bytes.to_string()),
                ]),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This host is expected to have real NVIDIA GPUs (confirmed via `nvidia-smi` before
    /// writing this test); on a host with no NVIDIA driver, `discover()` would return
    /// `Err(GpuError::Load(_))` instead, which is the documented, non-panicking failure mode
    /// for that case (exercised separately in CI-less/no-GPU environments by inspection, not
    /// by an automated test, since we can't uninstall the driver from under this test run).
    #[test]
    fn discover_finds_at_least_one_gpu_on_this_host() {
        let topology = GpuTopology::discover().expect("NVML should initialize on this host");
        assert!(
            !topology.devices().is_empty(),
            "expected at least one NVIDIA GPU on this host"
        );
        for gpu in topology.devices() {
            assert!(!gpu.name.is_empty());
            assert!(!gpu.uuid.is_empty());
            assert!(gpu.total_memory_bytes > 0);
        }
    }

    #[test]
    fn as_resources_projects_one_gpu_resource_per_device() {
        let topology = GpuTopology::discover().expect("NVML should initialize on this host");
        let resources = topology.as_resources("test-node");
        assert_eq!(resources.len(), topology.devices().len());
        for resource in &resources {
            assert_eq!(resource.kind, ResourceKind::Gpu);
            assert!(resource.exclusive, "GPUs should be leased exclusively");
            assert!(resource.attributes.contains_key("gpu.uuid"));
        }
    }
}
pub mod adapter;
