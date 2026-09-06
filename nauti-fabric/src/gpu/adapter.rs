//! `GpuLocalAdapter` — the host-local GPU reservation adapter.
//!
//! On a host where passthrough (VFIO-PCI) is not configured, a leased GPU
//! `Resource` is honored as a *reservation*: the fabric records that one
//! controller owns the lease, the GPU is reported as in-use to anything
//! that asks, and no real hardware is bound. This is the honest fallback
//! for c4's current setup (consumer NVIDIA cards, no IOMMU groups) and
//! for any host that has not opted into the `vfio` feature.
//!
//! When VFIO is available (separate `vmm::vfio` adapter, gated on
//! `--features vfio`), the resource is attached via the real passthrough
//! path. This adapter and the VFIO adapter are mutually exclusive at the
//! adapter level — an operator picks which to register, or registers both
//! and lets the request attributes decide.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::{AdapterReport, Attachment, FabricError, Lease, Resource, ResourceAdapter, ResourceKind};

/// Adapter name. Listed in `nauti adapters` as `gpu-local`.
pub const ADAPTER_NAME: &str = "gpu-local";

/// Scope string for the capability report. Distinct from the VFIO adapter's
/// `gpu-vfio` scope so an operator can see at a glance which GPU attach
/// path is wired.
pub const SCOPE: &str = "gpu-host-local";

/// Reserves a leased GPU as "in use by this lease" without binding any
/// hardware. Idempotent on re-attach.
#[derive(Default)]
pub struct GpuLocalAdapter {
    reservations: Mutex<HashMap<String, GpuReservation>>,
}

#[derive(Clone, Debug)]
struct GpuReservation {
    lease_id: u64,
    /// The GPU's stable identifier (UUID preferred; falls back to BDF) so
    /// we can detect a hot-swap between two attach calls. Stored now;
    /// consumed by the upcoming `inventory refresh` work that diffs the
    /// current GPU set against reservations to detect a card that moved
    /// BDFs while a lease was active.
    #[allow(dead_code)]
    gpu_id: String,
    /// When the reservation was created. Useful for an operator-driven
    /// "how long has this GPU been held" check; not yet read.
    #[allow(dead_code)]
    attached_at: std::time::Instant,
}

impl std::fmt::Debug for GpuLocalAdapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.reservations.lock().map(|m| m.len()).unwrap_or(0);
        formatter
            .debug_struct("GpuLocalAdapter")
            .field("reservations", &count)
            .finish()
    }
}

impl GpuLocalAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of currently-tracked reservations. Public for tests and for
    /// an operator who wants to confirm the adapter's view of in-use GPUs
    /// matches reality.
    #[cfg(test)]
    pub fn reservation_count(&self) -> usize {
        self.reservations.lock().map(|m| m.len()).unwrap_or(0)
    }
}

impl ResourceAdapter for GpuLocalAdapter {
    fn name(&self) -> &str {
        ADAPTER_NAME
    }

    fn attach(&self, resource: &Resource, lease: &Lease) -> Result<Attachment, FabricError> {
        if resource.kind != ResourceKind::Gpu {
            return Err(FabricError::IncompatibleResourceKind {
                adapter: self.name().into(),
                expected: ResourceKind::Gpu,
                actual: resource.kind,
            });
        }

        let gpu_id = resource
            .attributes
            .get("gpu.uuid")
            .cloned()
            .or_else(|| resource.attributes.get("gpu.pci_bus_id").cloned())
            .ok_or_else(|| {
                FabricError::MissingResourceAttribute("gpu.uuid or gpu.pci_bus_id".into())
            })?;

        let mut details = std::collections::BTreeMap::from([
            ("gpu.id".to_string(), gpu_id.clone()),
            ("gpu.attachment_scope".to_string(), SCOPE.into()),
            (
                "gpu.note".to_string(),
                "host-local reservation; no VFIO passthrough bound".into(),
            ),
            ("adapter.implementation".to_string(), ADAPTER_NAME.into()),
        ]);
        if let Some(name) = resource.attributes.get("gpu.name") {
            details.insert("gpu.name".to_string(), name.clone());
        }
        if let Some(bdf) = resource.attributes.get("gpu.pci_bus_id") {
            details.insert("gpu.pci_bus_id".to_string(), bdf.clone());
        }
        if let Some(free) = resource.attributes.get("gpu.free_memory_bytes") {
            details.insert("gpu.free_memory_bytes_at_attach".to_string(), free.clone());
        }

        {
            let guard = self.reservations.lock().expect("reservation lock poisoned");
            if let Some(existing) = guard.get(&resource.id) {
                if existing.lease_id == lease.id {
                    let mut existing_details = details.clone();
                    existing_details.insert("gpu.reattach".to_string(), "true".into());
                    return Ok(Attachment {
                        resource_id: resource.id.clone(),
                        lease_id: lease.id,
                        adapter: self.name().into(),
                        details: existing_details,
                    });
                }
            }
        }

        let reservation = GpuReservation {
            lease_id: lease.id,
            gpu_id,
            attached_at: std::time::Instant::now(),
        };
        self.reservations
            .lock()
            .expect("reservation lock poisoned")
            .insert(resource.id.clone(), reservation);

        Ok(Attachment {
            resource_id: resource.id.clone(),
            lease_id: lease.id,
            adapter: self.name().into(),
            details,
        })
    }

    fn capability_report(&self) -> AdapterReport {
        let reservation_count = self.reservations.lock().map(|m| m.len()).unwrap_or(0);
        // Live GPU count from the all-smi adapter — reflects whatever the
        // kernel currently reports, not a static config.
        let gpu_count = crate::gpu::GpuDiscoveryResult::discover()
            .map(|d| d.devices.iter().filter(|g| !g.display_only).count())
            .unwrap_or(0);
        AdapterReport {
            name: self.name().into(),
            scope: SCOPE.into(),
            healthy: true,
            detail: std::collections::BTreeMap::from([
                ("adapter.implementation".to_string(), ADAPTER_NAME.into()),
                (
                    "gpu.attachment_mode".to_string(),
                    "host-local reservation (no hardware binding)".into(),
                ),
                ("gpu.count_live".to_string(), gpu_count.to_string()),
                ("reservations.current".to_string(), reservation_count.to_string()),
                (
                    "vfio.available".to_string(),
                    "see vmm::vfio adapter if --features vfio is enabled".into(),
                ),
            ]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ResourceState;
    use std::collections::BTreeMap;

    fn gpu_resource(id: &str) -> Resource {
        Resource {
            id: id.into(),
            kind: ResourceKind::Gpu,
            capacity: 1,
            unit: "device".into(),
            node: "local".into(),
            state: ResourceState::Available,
            exclusive: true,
            attributes: BTreeMap::from([
                ("gpu.name".into(), "Test GPU".into()),
                ("gpu.uuid".into(), "GPU-deadbeef-0000-0000-0000-000000000000".into()),
                ("gpu.pci_bus_id".into(), "0000:01:00.0".into()),
                ("gpu.free_memory_bytes".into(), "16106127360".into()),
            ]),
        }
    }

    fn lease(id: u64, resource_id: &str) -> Lease {
        Lease { id, resource_id: resource_id.into(), owner: "test".into() }
    }

    #[test]
    fn attach_rejects_non_gpu_resource() {
        let adapter = GpuLocalAdapter::new();
        let mut resource = gpu_resource("gpu.0");
        resource.kind = ResourceKind::Storage;
        let err = adapter.attach(&resource, &lease(1, "gpu.0")).unwrap_err();
        assert_eq!(
            err,
            FabricError::IncompatibleResourceKind {
                adapter: ADAPTER_NAME.into(),
                expected: ResourceKind::Gpu,
                actual: ResourceKind::Storage,
            }
        );
    }

    #[test]
    fn attach_rejects_resource_without_uuid_or_bdf() {
        let adapter = GpuLocalAdapter::new();
        let mut resource = gpu_resource("gpu.0");
        resource.attributes.remove("gpu.uuid");
        resource.attributes.remove("gpu.pci_bus_id");
        let err = adapter.attach(&resource, &lease(1, "gpu.0")).unwrap_err();
        assert!(matches!(err, FabricError::MissingResourceAttribute(_)));
    }

    #[test]
    fn attach_accepts_resource_with_only_pci_bus_id() {
        let adapter = GpuLocalAdapter::new();
        let mut resource = gpu_resource("gpu.0");
        resource.attributes.remove("gpu.uuid");
        let attachment = adapter.attach(&resource, &lease(1, "gpu.0")).unwrap();
        assert_eq!(attachment.adapter, ADAPTER_NAME);
        assert_eq!(attachment.details["gpu.id"], "0000:01:00.0");
    }

    #[test]
    fn attach_is_idempotent_on_repeat_for_same_lease() {
        let adapter = GpuLocalAdapter::new();
        let resource = gpu_resource("gpu.0");
        let first = adapter.attach(&resource, &lease(1, "gpu.0")).unwrap();
        let second = adapter.attach(&resource, &lease(1, "gpu.0")).unwrap();
        assert_eq!(adapter.reservation_count(), 1, "second attach must not double-book");
        assert!(!first.details.contains_key("gpu.reattach"), "first attach is the original");
        assert_eq!(second.details["gpu.reattach"], "true", "second attach is marked");
    }

    #[test]
    fn attach_separate_resources_each_get_a_reservation() {
        let adapter = GpuLocalAdapter::new();
        let r0 = gpu_resource("gpu.0");
        let r1 = gpu_resource("gpu.1");
        adapter.attach(&r0, &lease(1, "gpu.0")).unwrap();
        adapter.attach(&r1, &lease(2, "gpu.1")).unwrap();
        assert_eq!(adapter.reservation_count(), 2);
    }

    #[test]
    fn capability_report_is_honest() {
        let adapter = GpuLocalAdapter::new();
        let report = adapter.capability_report();
        assert_eq!(report.name, ADAPTER_NAME);
        assert_eq!(report.scope, SCOPE);
        assert!(report.healthy, "GpuLocalAdapter has no external dependency; it is always healthy");
        assert!(report.detail.contains_key("gpu.attachment_mode"));
        assert!(report.detail["gpu.attachment_mode"].contains("no hardware binding"));
    }

    #[test]
    fn capability_report_reflects_current_reservation_count() {
        let adapter = GpuLocalAdapter::new();
        let r0 = gpu_resource("gpu.0");
        adapter.attach(&r0, &lease(1, "gpu.0")).unwrap();
        let report = adapter.capability_report();
        assert_eq!(report.detail["reservations.current"], "1");
    }
}
