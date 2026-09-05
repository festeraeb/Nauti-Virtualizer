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

        // For each PCI device, find its "local" NUMA node(s) by walking up the ancestor chain
        // until we find an object that carries a nodeset (PCI/IO objects don't have one of
        // their own; hwloc attaches nodesets to normal/Memory objects). The first bit set in
        // that nodeset is the NUMA node os_index this device is closest to. If no ancestor
        // carries a nodeset (e.g. a UMA system with no NUMA nodes at all), the device is left
        // unassigned here and picked up by the "no NUMA nodes" fallback below.
        let mut pci_by_node_index: BTreeMap<usize, Vec<PciDeviceReport>> = BTreeMap::new();
        let mut unassigned_pci_devices: Vec<PciDeviceReport> = Vec::new();
        for obj in topology.objects_with_type(ObjectType::PCIDevice) {
            let report = PciDeviceReport {
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
            };

            let local_node_index = std::iter::once(obj)
                .chain(obj.ancestors())
                .find_map(|ancestor| ancestor.nodeset())
                .and_then(|nodeset| nodeset.first_set())
                .map(|index| usize::from(index));

            match local_node_index {
                Some(index) => pci_by_node_index.entry(index).or_default().push(report),
                None => unassigned_pci_devices.push(report),
            }
        }

        let mut nodes = topology
            .objects_with_type(ObjectType::NUMANode)
            .map(|obj| {
                let os_index = obj.os_index().map(|i| i as usize).unwrap_or_default();
                NumaNodeReport {
                    os_index,
                    local_memory_bytes: obj.total_memory(),
                    logical_cpus: obj
                        .cpuset()
                        .map(|set| set.weight().unwrap_or(0) as usize)
                        .unwrap_or(0),
                    pci_devices: pci_by_node_index.remove(&os_index).unwrap_or_default(),
                }
            })
            .collect::<Vec<_>>();

        // Any PCI devices whose ancestor-walk didn't land on a node we actually enumerated
        // (nodeset math referenced an os_index we don't have a NUMANode object for) still get
        // reported, attached to the first known node so the facts aren't silently dropped.
        for (_, mut leftover) in pci_by_node_index {
            unassigned_pci_devices.append(&mut leftover);
        }

        if !unassigned_pci_devices.is_empty() {
            if let Some(first) = nodes.first_mut() {
                first.pci_devices.append(&mut unassigned_pci_devices);
            } else {
                // No NUMA nodes reported at all (e.g. single-node/UMA system) but PCI devices
                // exist: synthesize a single implicit node so the PCI facts aren't dropped.
                nodes.push(NumaNodeReport {
                    os_index: 0,
                    local_memory_bytes: 0,
                    logical_cpus: 0,
                    pci_devices: unassigned_pci_devices,
                });
            }
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

    #[test]
    fn pci_devices_are_distributed_across_numa_nodes_not_dropped() {
        // Sanity check for the ancestor-walk PCI attribution: every PCI device hwloc reports
        // must end up attached to exactly one NUMA node's report (none silently dropped),
        // whether or not this host actually has PCI devices or multiple NUMA nodes.
        let topology = NumaTopology::discover().expect("hwloc topology should load");
        let attributed_pci_count: usize =
            topology.nodes().iter().map(|node| node.pci_devices.len()).sum();

        let direct_pci_count = hwlocality::Topology::new()
            .expect("hwloc topology should load")
            .objects_with_type(hwlocality::object::types::ObjectType::PCIDevice)
            .count();

        assert_eq!(attributed_pci_count, direct_pci_count);
    }
}
