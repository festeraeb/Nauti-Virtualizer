//! Foundational resource model and local, exclusive lease proof of concept.

pub mod adapters;
pub mod gpu;
pub mod rpc;
pub mod rpc_auth;
pub mod config;
#[cfg(feature = "numa")]
pub mod topology;
#[cfg(feature = "cloud-hypervisor")]
pub mod vmm;

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sysinfo::{Disks, Networks, System};
use thiserror::Error;
use tracing::info;
use vm_memory::{GuestAddress, GuestMemoryMmap};

pub use adapters::{LemonadeAdapter, LemonadeConfig, LocalResourceAdapter, NetworkResourceAdapter};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum ResourceKind {
    Cpu,
    Gpu,
    Memory,
    Storage,
    Network,
    Device,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ResourceState {
    Available,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Resource {
    pub id: String,
    pub kind: ResourceKind,
    pub capacity: u64,
    pub unit: String,
    pub node: String,
    pub state: ResourceState,
    pub exclusive: bool,
    pub attributes: BTreeMap<String, String>,
}

impl Resource {
    pub fn local_cpu(node: impl Into<String>) -> Self {
        Self {
            id: "local.cpu.logical".into(),
            kind: ResourceKind::Cpu,
            capacity: std::thread::available_parallelism()
                .map_or(1, |parallelism| parallelism.get() as u64),
            unit: "logical-cpu".into(),
            node: node.into(),
            state: ResourceState::Available,
            exclusive: false,
            attributes: BTreeMap::from([("topology.scope".into(), "host".into())]),
        }
    }

    pub fn local_memory(node: impl Into<String>, bytes: u64) -> Self {
        Self {
            id: "local.memory".into(),
            kind: ResourceKind::Memory,
            capacity: bytes,
            unit: "bytes".into(),
            node: node.into(),
            state: ResourceState::Available,
            exclusive: false,
            attributes: BTreeMap::from([("topology.scope".into(), "host".into())]),
        }
    }
}

/// Portable local inventory collected without requiring privileged hardware access.
pub struct HostInventory;

impl HostInventory {
    pub fn discover(node: impl Into<String>) -> Vec<Resource> {
        let node = node.into();
        let system = System::new_all();
        let mut resources = vec![Resource::local_cpu(node.clone()), Resource::local_memory(
            node.clone(),
            system.total_memory(),
        )];

        let disks = Disks::new_with_refreshed_list();
        resources.extend(disks.list().iter().enumerate().map(|(index, disk)| Resource {
            id: format!("local.storage.{index}"),
            kind: ResourceKind::Storage,
            capacity: disk.total_space(),
            unit: "bytes".into(),
            node: node.clone(),
            state: ResourceState::Available,
            exclusive: false,
            attributes: BTreeMap::from([
                ("name".into(), disk.name().to_string_lossy().into_owned()),
                ("mount_point".into(), disk.mount_point().display().to_string()),
                ("available_bytes".into(), disk.available_space().to_string()),
            ]),
        }));

        let networks = Networks::new_with_refreshed_list();
        resources.extend(networks.list().iter().map(|(name, network)| Resource {
            id: format!("local.network.{name}"),
            kind: ResourceKind::Network,
            capacity: 0,
            unit: "unknown-bandwidth".into(),
            node: node.clone(),
            state: ResourceState::Available,
            exclusive: false,
            attributes: BTreeMap::from([
                ("interface".into(), name.clone()),
                ("mac_address".into(), network.mac_address().to_string()),
            ]),
        }));
        resources
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum FabricError {
    #[error("resource does not exist")]
    ResourceNotFound,
    #[error("resource is unavailable")]
    ResourceUnavailable,
    #[error("resource already has an active exclusive lease")]
    ResourceAlreadyLeased,
    #[error("lease does not exist or is not owned by this caller")]
    LeaseNotFound,
    #[error("adapter does not exist")]
    AdapterNotFound,
    #[error("adapter {adapter} requires {expected:?}, but resource is {actual:?}")]
    IncompatibleResourceKind {
        adapter: String,
        expected: ResourceKind,
        actual: ResourceKind,
    },
    #[error("adapter {adapter} requires a {expected} resource, but resource is {actual}")]
    IncompatibleResourceLocality {
        adapter: String,
        expected: String,
        actual: String,
    },
    #[error("resource does not contain required adapter attribute: {0}")]
    MissingResourceAttribute(String),
    #[error("adapter {adapter} backend is unavailable: {reason}")]
    AdapterBackendUnavailable { adapter: String, reason: String },
    #[error("guest memory allocation failed: {0}")]
    GuestMemory(String),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Lease {
    pub id: u64,
    pub resource_id: String,
    pub owner: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Attachment {
    pub resource_id: String,
    pub lease_id: u64,
    pub adapter: String,
    pub details: BTreeMap<String, String>,
}

/// A capability/health snapshot for a registered [`ResourceAdapter`].
///
/// Adapters are stateless attach-time strategies, so "health" here means
/// whether the adapter is currently able to accept attach calls (for
/// example, a network adapter could report unhealthy if its transport
/// dependency is unreachable). `scope` is a short machine-readable label
/// describing what the adapter operates on (`local-host`, `remote-descriptor`,
/// `proof-only`, ...), and `detail` carries adapter-specific diagnostics.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdapterReport {
    pub name: String,
    pub scope: String,
    pub healthy: bool,
    pub detail: BTreeMap<String, String>,
}

pub trait ResourceAdapter: Send + Sync {
    fn name(&self) -> &str;

    fn attach(&self, resource: &Resource, lease: &Lease) -> Result<Attachment, FabricError>;

    /// Reports this adapter's capability scope and current health.
    ///
    /// The default implementation assumes a stateless, always-healthy
    /// adapter; override it for adapters with external dependencies.
    fn capability_report(&self) -> AdapterReport {
        AdapterReport {
            name: self.name().into(),
            scope: "unspecified".into(),
            healthy: true,
            detail: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResourceRequest {
    pub kind: Option<ResourceKind>,
    pub minimum_capacity: Option<u64>,
    pub node: Option<String>,
    pub required_attributes: BTreeMap<String, String>,
    pub exclusive: bool,
}

impl ResourceRequest {
    fn matches(&self, resource: &Resource) -> bool {
        resource.state == ResourceState::Available
            && self.kind.is_none_or(|kind| kind == resource.kind)
            && self.minimum_capacity.is_none_or(|capacity| resource.capacity >= capacity)
            && self.node.as_ref().is_none_or(|node| node == &resource.node)
            && (!self.exclusive || resource.exclusive)
            && self
                .required_attributes
                .iter()
                .all(|(key, value)| resource.attributes.get(key) == Some(value))
    }
}

#[derive(Debug, Default)]
pub struct LocalProofAdapter;

impl ResourceAdapter for LocalProofAdapter {
    fn name(&self) -> &str {
        "local-proof"
    }

    fn attach(&self, resource: &Resource, lease: &Lease) -> Result<Attachment, FabricError> {
        if resource.id != lease.resource_id {
            return Err(FabricError::ResourceNotFound);
        }
        Ok(Attachment {
            resource_id: resource.id.clone(),
            lease_id: lease.id,
            adapter: self.name().into(),
            details: BTreeMap::new(),
        })
    }
}

#[derive(Debug)]
struct ActiveLease {
    lease: Lease,
    expires_at: Instant,
}

/// Result of a [`Fabric::refresh_local`] call. The operator (or a tool) can
/// inspect the diff to confirm what changed.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize)]
pub struct RefreshReport {
    /// Resource ids that were added by this refresh.
    pub added: Vec<String>,
    /// Resource ids that were removed (and were *not* currently leased).
    pub removed: Vec<String>,
    /// Resource ids that were *not* removed because they are currently
    /// leased; they will be removed by a future refresh once the lease
    /// expires, or by an explicit release + unregister.
    pub blocked_by_lease: Vec<String>,
}

#[derive(Default)]
pub struct Fabric {
    resources: RwLock<HashMap<String, Resource>>,
    leases: Mutex<HashMap<String, ActiveLease>>,
    adapters: RwLock<HashMap<String, Arc<dyn ResourceAdapter>>>,
    next_lease_id: AtomicU64,
}

impl Fabric {
    pub fn register(&self, resource: Resource) {
        self.resources
            .write()
            .expect("resource registry lock poisoned")
            .insert(resource.id.clone(), resource);
    }

    pub fn discover_local(&self, node: impl Into<String>) -> Vec<Resource> {
        let mut resources = HostInventory::discover(node);
        // Always discover GPUs via the all-smi adapter (any brand, live).
        if let Ok(discovery) = crate::gpu::GpuDiscoveryResult::discover() {
            resources.extend(discovery.as_resources(self.node_for_resources()));
        }
        for resource in &resources {
            self.register(resource.clone());
        }
        info!(resource_count = resources.len(), "registered local host inventory");
        resources
    }

    fn node_for_resources(&self) -> String {
        // Best-effort: use the system hostname so GPU resources are tagged
        // with the real node name rather than a caller-supplied default.
        std::env::var("NAUTI_NODE")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "local".into())
    }

    /// Remove a resource from the fabric. Returns `true` if the resource was
    /// registered and is now gone, `false` if no such resource id existed.
    ///
    /// If the resource has an active lease, the removal is **rejected** with
    /// [`FabricError::ResourceAlreadyLeased`]. The fabric does not silently
    /// orphan a lease; the operator must either wait for the lease to expire
    /// (default prune) or explicitly release it first. This invariant is
    /// tested in `unregister_rejects_a_leased_resource`.
    pub fn unregister(&self, resource_id: &str) -> Result<bool, FabricError> {
        // Lock order: leases first, then resources. The other Fabric methods
        // (lease_exclusive, release, attach) follow the same order, so this
        // can't deadlock against them.
        let leases = self.leases.lock().expect("lease registry lock poisoned");
        if leases.contains_key(resource_id) {
            return Err(FabricError::ResourceAlreadyLeased);
        }
        drop(leases);

        let removed = self
            .resources
            .write()
            .expect("resource registry lock poisoned")
            .remove(resource_id)
            .is_some();
        if removed {
            info!(resource_id, "resource unregistered");
        }
        Ok(removed)
    }

    /// Re-discover local host resources and diff against the current set.
    /// Resources present in the new discovery but missing from the fabric
    /// are registered. Resources present in the fabric but missing from the
    /// new discovery are removed (subject to the lease check in
    /// [`Self::unregister`]). The returned [`RefreshReport`] lists the ids
    /// added and removed so the operator (or a tool) can audit the change.
    ///
    /// GPU resources are re-discovered on every call via the all-smi adapter
    /// ([`crate::gpu::GpuDiscoveryResult`]), which walks `/sys/class/drm` and
    /// picks up NVIDIA, AMD, Intel, and any other GPU the kernel drives — no
    /// per-host config, no static maps. A hot-swapped card appears without an
    /// agent restart. When the `nvidia` feature is enabled, NVIDIA cards are
    /// additionally enriched with NVML telemetry (real name, UUID, utilization).
    pub fn refresh_local(&self, node: impl Into<String>) -> RefreshReport {
        let node = node.into();
        let mut new_resources = HostInventory::discover(node.clone());

        // Always discover GPUs via the all-smi adapter (any brand, live).
        // This is the self-discovering path: no per-host config, the next
        // call reflects whatever the kernel currently reports.
        match crate::gpu::GpuDiscoveryResult::discover() {
            Ok(discovery) => {
                let gpu_resources = discovery.as_resources(node.clone());
                info!(count = gpu_resources.len(), "refresh: discovered GPUs (all-smi)");
                new_resources.extend(gpu_resources);
            }
            Err(error) => {
                // Missing /sys/class/drm is unusual but not fatal; log and
                // continue with the host inventory.
                info!(?error, "refresh: GPU discovery unavailable, skipping");
            }
        }

        let new_ids: std::collections::BTreeSet<String> =
            new_resources.iter().map(|r| r.id.clone()).collect();
        let current_ids: std::collections::BTreeSet<String> = self
            .resources
            .read()
            .expect("resource registry lock poisoned")
            .keys()
            .cloned()
            .collect();

        let added: Vec<String> = new_ids.difference(&current_ids).cloned().collect();
        let removed: Vec<String> = current_ids.difference(&new_ids).cloned().collect();

        // Add the new ones first so an operator that calls refresh and
        // then immediately queries sees the new resources.
        for resource in &new_resources {
            if added.contains(&resource.id) {
                self.register(resource.clone());
            }
        }
        // Then remove the gone ones. Skip any that are currently leased
        // (collect them into a separate list so we don't try-and-fail in
        // a loop — `unregister` returns the error on the first hit).
        let mut removed_unblocked: Vec<String> = Vec::new();
        let mut blocked_by_lease: Vec<String> = Vec::new();
        for id in &removed {
            match self.unregister(id) {
                Ok(true) => removed_unblocked.push(id.clone()),
                Ok(false) => { /* not present, nothing to do */ }
                Err(FabricError::ResourceAlreadyLeased) => {
                    blocked_by_lease.push(id.clone());
                    info!(
                        resource_id = %id,
                        "refresh: resource disappeared but is currently leased; leaving in registry until lease expires"
                    );
                }
                Err(other) => {
                    info!(resource_id = %id, error = ?other, "refresh: unregister failed");
                }
            }
        }

        info!(
            added = added.len(),
            removed = removed_unblocked.len(),
            blocked_by_lease = blocked_by_lease.len(),
            "refresh complete"
        );

        RefreshReport {
            added,
            removed: removed_unblocked,
            blocked_by_lease,
        }
    }

    pub fn register_adapter(&self, adapter: Arc<dyn ResourceAdapter>) {
        self.adapters
            .write()
            .expect("adapter registry lock poisoned")
            .insert(adapter.name().into(), adapter);
    }

    /// Collects a capability/health report from every registered adapter.
    pub fn adapter_reports(&self) -> Vec<AdapterReport> {
        let mut reports: Vec<_> = self
            .adapters
            .read()
            .expect("adapter registry lock poisoned")
            .values()
            .map(|adapter| adapter.capability_report())
            .collect();
        reports.sort_by(|left, right| left.name.cmp(&right.name));
        reports
    }

    pub fn resource(&self, resource_id: &str) -> Option<Resource> {
        self.resources
            .read()
            .expect("resource registry lock poisoned")
            .get(resource_id)
            .cloned()
    }

    pub fn resources(&self) -> Vec<Resource> {
        let mut resources: Vec<_> = self
            .resources
            .read()
            .expect("resource registry lock poisoned")
            .values()
            .cloned()
            .collect();
        resources.sort_by(|left, right| left.id.cmp(&right.id));
        resources
    }

    pub fn lease_exclusive(
        &self,
        resource_id: &str,
        owner: impl Into<String>,
        ttl: Duration,
    ) -> Result<Lease, FabricError> {
        let resource = self.resource(resource_id).ok_or(FabricError::ResourceNotFound)?;
        if resource.state != ResourceState::Available {
            return Err(FabricError::ResourceUnavailable);
        }
        if !resource.exclusive {
            return Err(FabricError::ResourceUnavailable);
        }

        let mut leases = self.leases.lock().expect("lease registry lock poisoned");
        leases.retain(|_, active| active.expires_at > Instant::now());
        if leases.contains_key(resource_id) {
            return Err(FabricError::ResourceAlreadyLeased);
        }

        let lease = Lease {
            id: self.next_lease_id.fetch_add(1, Ordering::Relaxed) + 1,
            resource_id: resource_id.into(),
            owner: owner.into(),
        };
        leases.insert(
            resource_id.into(),
            ActiveLease {
                lease: lease.clone(),
                expires_at: Instant::now() + ttl,
            },
        );
        info!(resource_id, lease_id = lease.id, "exclusive resource leased");
        Ok(lease)
    }

    pub fn find_available(&self, request: &ResourceRequest) -> Vec<Resource> {
        let leases = self.leases.lock().expect("lease registry lock poisoned");
        self.resources
            .read()
            .expect("resource registry lock poisoned")
            .values()
            .filter(|resource| {
                request.matches(resource)
                    && (!resource.exclusive || !leases.contains_key(&resource.id))
            })
            .cloned()
            .collect()
    }

    pub fn attach(&self, adapter_name: &str, lease: &Lease) -> Result<Attachment, FabricError> {
        let resource = self.resource(&lease.resource_id).ok_or(FabricError::ResourceNotFound)?;
        let leases = self.leases.lock().expect("lease registry lock poisoned");
        let authorized = leases
            .get(&lease.resource_id)
            .is_some_and(|active| active.lease.id == lease.id && active.expires_at > Instant::now());
        if !authorized {
            return Err(FabricError::LeaseNotFound);
        }
        let adapter = self
            .adapters
            .read()
            .expect("adapter registry lock poisoned")
            .get(adapter_name)
            .cloned()
            .ok_or(FabricError::AdapterNotFound)?;
        adapter.attach(&resource, lease)
    }

    pub fn release(&self, lease: &Lease) -> Result<(), FabricError> {
        let mut leases = self.leases.lock().expect("lease registry lock poisoned");
        match leases.get(&lease.resource_id) {
            Some(active) if active.lease.id == lease.id => {
                leases.remove(&lease.resource_id);
                info!(resource_id = lease.resource_id, lease_id = lease.id, "resource released");
                Ok(())
            }
            _ => Err(FabricError::LeaseNotFound),
        }
    }

    /// Extends an active lease's expiry by `ttl` from now, provided the
    /// caller presents the exact lease that is currently active (and not
    /// already expired) for that resource. Renewing does not change the
    /// lease `id`, so any already-attached [`Attachment`] remains valid.
    pub fn renew_lease(&self, lease: &Lease, ttl: Duration) -> Result<Lease, FabricError> {
        let mut leases = self.leases.lock().expect("lease registry lock poisoned");
        leases.retain(|_, active| active.expires_at > Instant::now());
        match leases.get_mut(&lease.resource_id) {
            Some(active) if active.lease.id == lease.id => {
                active.expires_at = Instant::now() + ttl;
                info!(resource_id = lease.resource_id, lease_id = lease.id, "lease renewed");
                Ok(active.lease.clone())
            }
            _ => Err(FabricError::LeaseNotFound),
        }
    }
}

/// Allocates guest-addressable RAM using rust-vmm's `vm-memory` implementation.
pub fn allocate_guest_memory(bytes: usize) -> Result<GuestMemoryMmap<()>, FabricError> {
    GuestMemoryMmap::from_ranges(&[(GuestAddress(0), bytes)])
        .map_err(|error| FabricError::GuestMemory(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusive_resource_cannot_be_double_booked_and_can_be_released() {
        let fabric = Fabric::default();
        fabric.register(Resource {
            id: "gpu.local.0".into(),
            kind: ResourceKind::Gpu,
            capacity: 1,
            unit: "device".into(),
            node: "local".into(),
            state: ResourceState::Available,
            exclusive: true,
            attributes: BTreeMap::new(),
        });

        let lease = fabric
            .lease_exclusive("gpu.local.0", "mission-a", Duration::from_secs(30))
            .expect("first lease should succeed");
        assert_eq!(
            fabric.lease_exclusive("gpu.local.0", "mission-b", Duration::from_secs(30)),
            Err(FabricError::ResourceAlreadyLeased)
        );

        fabric.release(&lease).expect("owner can release lease");
        assert!(fabric
            .lease_exclusive("gpu.local.0", "mission-b", Duration::from_secs(30))
            .is_ok());
    }

    #[test]
    fn local_cpu_discovery_registers_a_usable_resource() {
        let resource = Resource::local_cpu("local");
        assert_eq!(resource.kind, ResourceKind::Cpu);
        assert!(resource.capacity >= 1);
    }

    #[test]
    fn adapter_attachment_carries_the_authorized_lease() {
        let resource = Resource {
            id: "storage.local.0".into(),
            kind: ResourceKind::Storage,
            capacity: 1,
            unit: "device".into(),
            node: "local".into(),
            state: ResourceState::Available,
            exclusive: true,
            attributes: BTreeMap::new(),
        };
        let lease = Lease {
            id: 42,
            resource_id: resource.id.clone(),
            owner: "mission-a".into(),
        };

        let attachment = LocalProofAdapter.attach(&resource, &lease).unwrap();
        assert_eq!(attachment.lease_id, lease.id);
        assert_eq!(attachment.adapter, "local-proof");
        assert!(attachment.details.is_empty());
    }

    #[test]
    fn host_inventory_includes_cpu_and_memory_resources() {
        let resources = HostInventory::discover("test-host");
        assert!(resources.iter().any(|resource| resource.kind == ResourceKind::Cpu));
        assert!(resources.iter().any(|resource| resource.kind == ResourceKind::Memory));
    }

    #[test]
    fn resources_are_returned_in_stable_order() {
        let fabric = Fabric::default();
        fabric.register(Resource::local_cpu("local"));
        fabric.register(Resource::local_memory("local", 1024));
        let resource_ids: Vec<_> = fabric.resources().into_iter().map(|resource| resource.id).collect();
        assert_eq!(resource_ids, ["local.cpu.logical", "local.memory"]);
    }

    #[test]
    fn scheduler_filters_on_resource_capabilities() {
        let fabric = Fabric::default();
        fabric.register(Resource {
            id: "gpu.local.0".into(),
            kind: ResourceKind::Gpu,
            capacity: 24,
            unit: "GiB".into(),
            node: "local".into(),
            state: ResourceState::Available,
            exclusive: true,
            attributes: BTreeMap::from([("vendor".into(), "nvidia".into())]),
        });

        let matches = fabric.find_available(&ResourceRequest {
            kind: Some(ResourceKind::Gpu),
            minimum_capacity: Some(16),
            required_attributes: BTreeMap::from([("vendor".into(), "nvidia".into())]),
            exclusive: true,
            ..ResourceRequest::default()
        });
        assert_eq!(matches.len(), 1);
    }

    #[test]
    fn registered_adapter_requires_an_active_lease() {
        let fabric = Fabric::default();
        fabric.register(Resource {
            id: "device.local.0".into(),
            kind: ResourceKind::Device,
            capacity: 1,
            unit: "device".into(),
            node: "local".into(),
            state: ResourceState::Available,
            exclusive: true,
            attributes: BTreeMap::new(),
        });
        fabric.register_adapter(Arc::new(LocalProofAdapter));
        let lease = fabric
            .lease_exclusive("device.local.0", "mission-a", Duration::from_secs(30))
            .unwrap();

        assert_eq!(fabric.attach("local-proof", &lease).unwrap().lease_id, lease.id);
    }

    fn register_gpu(fabric: &Fabric) {
        fabric.register(Resource {
            id: "gpu.local.0".into(),
            kind: ResourceKind::Gpu,
            capacity: 1,
            unit: "device".into(),
            node: "local".into(),
            state: ResourceState::Available,
            exclusive: true,
            attributes: BTreeMap::new(),
        });
    }

    #[test]
    fn expired_lease_is_pruned_and_resource_becomes_available_again() {
        let fabric = Fabric::default();
        register_gpu(&fabric);

        fabric
            .lease_exclusive("gpu.local.0", "mission-a", Duration::from_millis(20))
            .expect("first lease should succeed");
        std::thread::sleep(Duration::from_millis(60));

        assert!(fabric
            .lease_exclusive("gpu.local.0", "mission-b", Duration::from_secs(30))
            .is_ok());
    }

    #[test]
    fn attach_rejects_a_lease_that_has_already_expired() {
        let fabric = Fabric::default();
        register_gpu(&fabric);
        fabric.register_adapter(Arc::new(LocalProofAdapter));

        let lease = fabric
            .lease_exclusive("gpu.local.0", "mission-a", Duration::from_millis(20))
            .expect("first lease should succeed");
        std::thread::sleep(Duration::from_millis(60));

        assert_eq!(fabric.attach("local-proof", &lease), Err(FabricError::LeaseNotFound));
    }

    #[test]
    fn renewing_a_lease_extends_it_past_its_original_ttl() {
        let fabric = Fabric::default();
        register_gpu(&fabric);
        fabric.register_adapter(Arc::new(LocalProofAdapter));

        let lease = fabric
            .lease_exclusive("gpu.local.0", "mission-a", Duration::from_millis(40))
            .expect("first lease should succeed");

        std::thread::sleep(Duration::from_millis(20));
        let renewed = fabric
            .renew_lease(&lease, Duration::from_millis(200))
            .expect("active lease can be renewed");
        assert_eq!(renewed.id, lease.id);

        // Past the *original* ttl, but the renewal should keep it alive.
        std::thread::sleep(Duration::from_millis(40));
        assert!(fabric.attach("local-proof", &lease).is_ok());
        assert_eq!(
            fabric.lease_exclusive("gpu.local.0", "mission-b", Duration::from_secs(30)),
            Err(FabricError::ResourceAlreadyLeased)
        );
    }

    #[test]
    fn renewing_an_expired_lease_fails() {
        let fabric = Fabric::default();
        register_gpu(&fabric);

        let lease = fabric
            .lease_exclusive("gpu.local.0", "mission-a", Duration::from_millis(20))
            .expect("first lease should succeed");
        std::thread::sleep(Duration::from_millis(60));

        assert_eq!(
            fabric.renew_lease(&lease, Duration::from_secs(30)),
            Err(FabricError::LeaseNotFound)
        );
    }

    #[test]
    fn renewing_an_unknown_lease_fails() {
        let fabric = Fabric::default();
        register_gpu(&fabric);
        let bogus = Lease { id: 999, resource_id: "gpu.local.0".into(), owner: "nobody".into() };
        assert_eq!(
            fabric.renew_lease(&bogus, Duration::from_secs(30)),
            Err(FabricError::LeaseNotFound)
        );
    }

    #[test]
    fn adapter_reports_are_sorted_and_include_default_scope() {
        let fabric = Fabric::default();
        fabric.register_adapter(Arc::new(LocalProofAdapter));
        fabric.register_adapter(Arc::new(LocalResourceAdapter));
        fabric.register_adapter(Arc::new(NetworkResourceAdapter));

        let reports = fabric.adapter_reports();
        let names: Vec<_> = reports.iter().map(|report| report.name.as_str()).collect();
        assert_eq!(names, ["local-proof", "local-resource", "network-resource"]);
        assert!(reports.iter().all(|report| report.healthy));
        assert_eq!(
            reports.iter().find(|report| report.name == "local-proof").unwrap().scope,
            "unspecified"
        );
    }

    #[test]
    fn rust_vmm_guest_memory_allocation_is_usable() {
        assert!(allocate_guest_memory(1024 * 1024).is_ok());
    }

    // -----------------------------------------------------------------------
    // unregister + refresh_local
    // -----------------------------------------------------------------------

    fn register_test_resource(fabric: &Fabric, id: &str) {
        fabric.register(Resource {
            id: id.into(),
            kind: ResourceKind::Device,
            capacity: 1,
            unit: "device".into(),
            node: "test-node".into(),
            state: ResourceState::Available,
            exclusive: true,
            attributes: BTreeMap::new(),
        });
    }

    #[test]
    fn unregister_of_nonexistent_resource_returns_false() {
        let fabric = Fabric::default();
        // No prior registration; unregister is a no-op success.
        assert_eq!(fabric.unregister("nope").unwrap(), false);
    }

    #[test]
    fn unregister_removes_a_registered_resource() {
        let fabric = Fabric::default();
        register_test_resource(&fabric, "device.0");
        assert!(fabric.resource("device.0").is_some());
        assert_eq!(fabric.unregister("device.0").unwrap(), true);
        assert!(fabric.resource("device.0").is_none());
    }

    #[test]
    fn unregister_rejects_a_leased_resource() {
        let fabric = Fabric::default();
        register_test_resource(&fabric, "device.0");
        let lease = fabric
            .lease_exclusive("device.0", "mission-a", Duration::from_secs(30))
            .unwrap();
        // unregister must refuse to orphan the lease.
        assert_eq!(
            fabric.unregister("device.0").unwrap_err(),
            FabricError::ResourceAlreadyLeased
        );
        // The resource is still registered.
        assert!(fabric.resource("device.0").is_some());
        // After release, unregister succeeds.
        fabric.release(&lease).unwrap();
        assert_eq!(fabric.unregister("device.0").unwrap(), true);
    }

    #[test]
    fn refresh_local_on_empty_fabric_reports_no_changes() {
        // We can't call refresh_local and assert it's empty because the
        // host's real HostInventory::discover will populate it. But we can
        // at least assert that a refresh runs without panic and returns a
        // valid report.
        let fabric = Fabric::default();
        let report = fabric.refresh_local("test-node");
        // The current host's resources are now in the fabric; `report.added`
        // should be exactly the resource ids that were added (which is
        // every discovered resource, since we started empty).
        let current: std::collections::BTreeSet<String> =
            fabric.resources().into_iter().map(|r| r.id).collect();
        let added: std::collections::BTreeSet<String> = report.added.iter().cloned().collect();
        assert_eq!(current, added, "every resource on this host was added");
    }

    #[test]
    fn refresh_local_is_idempotent_on_a_second_call() {
        let fabric = Fabric::default();
        let first = fabric.refresh_local("test-node");
        let second = fabric.refresh_local("test-node");
        // The first call adds every host resource. The second call adds
        // nothing (everything is already there) and removes nothing.
        assert!(second.added.is_empty(), "second refresh adds nothing");
        assert!(second.removed.is_empty(), "second refresh removes nothing");
        assert!(second.blocked_by_lease.is_empty());
        // Sanity: the first call did at least add *something* on this host
        // (CPU + memory at minimum).
        assert!(!first.added.is_empty(), "first refresh added host resources");
    }
}
