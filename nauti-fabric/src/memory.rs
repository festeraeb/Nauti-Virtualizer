//! Research memory plane: CXL / DAX / famfs / DRust.
//!
//! This module is how the bucket closes without lying. The fabric *discovers*
//! what the kernel exported and *refuses* to attach it as coherent live RAM.
//! DRust is an OSDI 2024 artifact, not a nauti backend.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{
    AdapterReport, FabricError, Lease, Resource, ResourceAdapter, ResourceKind, ResourceState,
};

pub const ADAPTER_NAME: &str = "memory-research";
pub const SCOPE: &str = "cxl-dax-famfs-drust-research";

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResearchMemoryReport {
    pub cxl_bus: bool,
    pub cxl_devices: Vec<String>,
    pub dax_nodes: Vec<String>,
    pub famfs_mounts: Vec<String>,
    pub drust_present: bool,
    pub claimed_as_live_ram: bool,
    pub notes: String,
}

impl ResearchMemoryReport {
    pub fn discover() -> Self {
        let cxl_root = Path::new("/sys/bus/cxl");
        let mut cxl_devices = Vec::new();
        if cxl_root.is_dir() {
            if let Ok(entries) = fs::read_dir(cxl_root.join("devices")) {
                for entry in entries.flatten() {
                    cxl_devices.push(entry.file_name().to_string_lossy().into_owned());
                }
            }
        }
        let mut dax_nodes = Vec::new();
        if let Ok(entries) = fs::read_dir("/dev") {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with("dax") {
                    dax_nodes.push(format!("/dev/{name}"));
                }
            }
        }
        let mut famfs_mounts = Vec::new();
        if let Ok(mounts) = fs::read_to_string("/proc/mounts") {
            for line in mounts.lines() {
                if line.contains("famfs") {
                    famfs_mounts.push(line.to_string());
                }
            }
        }
        let drust_present = Path::new("/opt/drust").exists()
            || Path::new("/usr/local/lib/libdrust.so").exists()
            || std::env::var_os("DRUST_HOME").is_some();
        Self {
            cxl_bus: cxl_root.is_dir(),
            cxl_devices,
            dax_nodes,
            famfs_mounts,
            drust_present,
            claimed_as_live_ram: false,
            notes: "Research only. Fabric will not attach these as coherent live memory."
                .into(),
        }
    }

    pub fn as_resource(&self, node: impl Into<String>) -> Resource {
        Resource {
            id: "local.memory.research".into(),
            kind: ResourceKind::Memory,
            capacity: 0,
            unit: "research".into(),
            node: node.into(),
            state: ResourceState::Unavailable,
            exclusive: true,
            attributes: BTreeMap::from([
                ("memory.kind".into(), "research".into()),
                ("memory.cxl".into(), self.cxl_bus.to_string()),
                ("memory.dax_count".into(), self.dax_nodes.len().to_string()),
                ("memory.famfs_count".into(), self.famfs_mounts.len().to_string()),
                ("memory.drust".into(), self.drust_present.to_string()),
                ("memory.claimed".into(), "false".into()),
            ]),
        }
    }
}

#[derive(Debug, Default)]
pub struct ResearchMemoryAdapter;

impl ResourceAdapter for ResearchMemoryAdapter {
    fn name(&self) -> &str {
        ADAPTER_NAME
    }

    fn attach(
        &self,
        resource: &Resource,
        _lease: &Lease,
    ) -> Result<crate::Attachment, FabricError> {
        let report = ResearchMemoryReport::discover();
        Err(FabricError::AdapterBackendUnavailable {
            adapter: ADAPTER_NAME.into(),
            reason: format!(
                "refusing live attach of {} (cxl={} dax={} famfs={} drust={}); \
                 research bucket is typed, never claimed",
                resource.id,
                report.cxl_bus,
                report.dax_nodes.len(),
                report.famfs_mounts.len(),
                report.drust_present
            ),
        })
    }

    fn capability_report(&self) -> AdapterReport {
        let report = ResearchMemoryReport::discover();
        AdapterReport {
            name: ADAPTER_NAME.into(),
            scope: SCOPE.into(),
            healthy: true,
            detail: BTreeMap::from([
                ("claimed_as_live_ram".into(), "false".into()),
                ("cxl_bus".into(), report.cxl_bus.to_string()),
                ("dax_nodes".into(), report.dax_nodes.len().to_string()),
                ("famfs_mounts".into(), report.famfs_mounts.len().to_string()),
                ("drust_present".into(), report.drust_present.to_string()),
            ]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Lease;

    #[test]
    fn discover_never_claims_live_ram() {
        let report = ResearchMemoryReport::discover();
        assert!(!report.claimed_as_live_ram);
        assert_eq!(report.as_resource("t440").state, ResourceState::Unavailable);
    }

    #[test]
    fn attach_is_typed_refuse() {
        let adapter = ResearchMemoryAdapter;
        let resource = ResearchMemoryReport::discover().as_resource("t440");
        let err = adapter
            .attach(
                &resource,
                &Lease {
                    id: 1,
                    resource_id: resource.id.clone(),
                    owner: "test".into(),
                },
            )
            .unwrap_err();
        match err {
            FabricError::AdapterBackendUnavailable { adapter, reason } => {
                assert_eq!(adapter, ADAPTER_NAME);
                assert!(reason.contains("never claimed"));
            }
            other => panic!("expected refuse, got {other:?}"),
        }
    }
}
