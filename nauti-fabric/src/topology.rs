//! Optional NUMA/PCI topology capture via `hwloc` (through the `hwlocality` crate).
//!
//! This module is compiled only when the `numa` feature is enabled, since it links against
//! the native `libhwloc` shared library (available as `libhwloc-dev`/`hwloc-devel` on most
//! distros, or vendored via `hwlocality`'s `vendored` feature). It is intentionally kept
//! separate from `HostInventory::discover` (which has no native dependencies) so that the
//! default build stays dependency-light; callers opt in explicitly by calling
//! [`NumaTopology::discover`].

use std::collections::BTreeMap;

use hwlocality::Topology;
use hwlocality::object::types::ObjectType;
use serde::{Deserialize, Serialize};

use crate::{Resource, ResourceKind, ResourceState};

/// A single NUMA node discovered via `hwloc`, with its associated logical CPU count and any
/// PCI devices found underneath it in the topology.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NumaNodeReport {
    pub os_index: usize,
    pub local_memory_bytes: u64,
    pub logical_cpus: usize,
    pub pci_devices: Vec<PciDeviceReport>,
}

/// A PCI device discovered via `hwloc`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PciDeviceReport {
    pub name: String,
    pub vendor_id: Option<String>,
    pub device_id: Option<String>,
}

/// Errors that can occur while probing the local hardware topology.
#[derive(Debug, thiserror::Error)]
pub enum TopologyError {
    #[error("failed to load hwloc topology: {0}")]
    Load(String),
}

/// Thin wrapper around an `hwlocality::Topology` capture, exposing just the NUMA/PCI facts
/// that are useful for resource-adapter capability reporting.
pub struct NumaTopology {
    nodes: Vec<NumaNodeReport>,
}

impl NumaTopology {
    /// Load the local hardware topology via `hwloc`. Requires `libhwloc` to be present on the
    /// system (or the crate's `vendored` feature to be enabled).
    pub fn discover() -> Result<Self, TopologyError> {
        let topology = Topology::new().map_err(|e| TopologyError::Load(e.to_string()))?;

        let pci_devices: Vec<PciDeviceReport> = topology
            .objects_with_type(ObjectType::PCIDevice)
            .map(|obj| PciDeviceReport {
                name: obj
                    .name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "unknown-pci-device".into()),
                vendor_id: obj
                    .info("PCIVendor")
                    .map(|v| v.to_string_lossy().into_owned()),
                device_id: obj
                    .info("PCIDevice")
                    .map(|v| v.to_string_lossy().into_owned()),
            })
            .collect();

        let nodes = topology
            .objects_with_type(ObjectType::NUMANode)
            .map(|obj| NumaNodeReport {
                os_index: obj.os_index().map(|i| i as usize).unwrap_or_default(),
                local_memory_bytes: obj.total_memory(),
                logical_cpus: obj
                    .cpuset()
                    .map(|set| set.weight().unwrap_or(0) as usize)
                    .unwrap_or(0),
                // Attributing specific PCI devices to specific NUMA nodes requires walking
                // ancestor chains; kept flat (all devices reported once, on node 0) for this
                // first pass to avoid over-engineering ahead of a real consumer.
                pci_devices: Vec::new(),
            })
            .collect::<Vec<_>>();

        let mut nodes = nodes;
        if let Some(first) = nodes.first_mut() {
            first.pci_devices = pci_devices;
        } else if !pci_devices.is_empty() {
            // No NUMA nodes reported (e.g. single-node/UMA system) but PCI devices exist:
            // synthesize a single implicit node so the PCI facts aren't dropped.
            nodes.push(NumaNodeReport {
                os_index: 0,
                local_memory_bytes: 0,
                logical_cpus: 0,
                pci_devices,
            });
        }

        Ok(Self { nodes })
    }

    pub fn nodes(&self) -> &[NumaNodeReport] {
        &self.nodes
    }

    /// Convert discovered NUMA nodes into `Resource` entries (kind `Memory`, one per node),
    /// tagged with topology attributes so callers can distinguish NUMA-local allocations from
    /// the coarse, single `local.memory` resource reported by `HostInventory::discover`.
    pub fn as_resources(&self, node: impl Into<String>) -> Vec<Resource> {
        let node = node.into();
        self.nodes
            .iter()
            .map(|numa| Resource {
                id: format!("local.numa.{}", numa.os_index),
                kind: ResourceKind::Memory,
                capacity: numa.local_memory_bytes,
                unit: "bytes".into(),
                node: node.clone(),
                state: ResourceState::Available,
                exclusive: false,
                attributes: BTreeMap::from([
                    ("topology.scope".into(), "numa-node".into()),
                    ("numa.os_index".into(), numa.os_index.to_string()),
                    ("numa.logical_cpus".into(), numa.logical_cpus.to_string()),
                    ("numa.pci_device_count".into(), numa.pci_devices.len().to_string()),
                ]),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_does_not_error_on_this_host() {
        // hwloc always succeeds on any real machine (worst case: a single "Machine" node with
        // no children), so this should never fail in CI or locally.
        let topology = NumaTopology::discover().expect("hwloc topology should load");
        // We don't assert on node count since CI runners may have 0 NUMA nodes reported
        // (UMA systems) - just confirm the call is infallible and resources convert cleanly.
        let resources = topology.as_resources("local");
        assert_eq!(resources.len(), topology.nodes().len());
        for resource in &resources {
            assert_eq!(resource.kind, ResourceKind::Memory);
            assert_eq!(resource.node, "local");
            assert!(resource.attributes.contains_key("numa.os_index"));
        }
    }
}
