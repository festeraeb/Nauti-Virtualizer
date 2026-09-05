//! Foundational resource model and local, exclusive lease proof of concept.

pub mod adapters;
pub mod rpc;

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sysinfo::{Disks, Networks, System};
use thiserror::Error;
use tracing::info;
use vm_memory::{GuestAddress, GuestMemoryMmap};

pub use adapters::{LocalResourceAdapter, NetworkResourceAdapter};

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

pub trait ResourceAdapter: Send + Sync {
    fn name(&self) -> &str;

    fn attach(&self, resource: &Resource, lease: &Lease) -> Result<Attachment, FabricError>;
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
        let resources = HostInventory::discover(node);
        for resource in &resources {
            self.register(resource.clone());
        }
        info!(resource_count = resources.len(), "registered local host inventory");
        resources
    }

    pub fn register_adapter(&self, adapter: Arc<dyn ResourceAdapter>) {
        self.adapters
            .write()
            .expect("adapter registry lock poisoned")
            .insert(adapter.name().into(), adapter);
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

    #[test]
    fn rust_vmm_guest_memory_allocation_is_usable() {
        assert!(allocate_guest_memory(1024 * 1024).is_ok());
    }
}
