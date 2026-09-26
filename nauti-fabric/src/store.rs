//! M8 — shared filesystem story.
//!
//! Declared decision, not a silent third option:
//!
//! * **Guest** attach uses Cloud Hypervisor virtio-fs (`--fs tag=,socket=`).
//! * **Host / job** attach uses the same directory as a bind path.
//! * **Cross-node** uses NFS only when `store.kind=nfs` is set.
//!
//! A job contract names `store.input` = resource id. Resolution is always
//! that store's `store.path` (or `store.nfs_export`). Same id, same path,
//! any node that registered the store. [`FabricStoreAdapter::detach`] runs
//! on lease release so the mount/lease does not linger.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::{
    AdapterReport, Attachment, FabricError, Lease, Resource, ResourceAdapter, ResourceKind,
    ResourceState,
};

/// How a fabric store is reached. The kind is an attribute, not a guess.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreKind {
    /// Host bind path. Jobs on the node read/write `store.path` directly.
    Bind,
    /// Guest virtio-fs. Cloud Hypervisor `--fs tag=,socket=`; virtiofsd
    /// (or any vhost-user-fs backend) serves `store.path`.
    VirtioFs,
    /// Cross-node NFS export. Only when the resource says so.
    Nfs,
}

impl StoreKind {
    pub fn parse(raw: &str) -> Result<Self, FabricError> {
        match raw {
            "bind" => Ok(Self::Bind),
            "virtio-fs" | "virtiofs" | "fs" => Ok(Self::VirtioFs),
            "nfs" => Ok(Self::Nfs),
            other => Err(FabricError::MissingResourceAttribute(format!(
                "store.kind (unknown {other}; expected bind|virtio-fs|nfs)"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bind => "bind",
            Self::VirtioFs => "virtio-fs",
            Self::Nfs => "nfs",
        }
    }
}

/// Resolved filesystem input for a job contract. Identical on every node
/// that registered the same store id.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreResolution {
    pub resource_id: String,
    pub kind: StoreKind,
    pub path: PathBuf,
    pub tag: String,
    pub socket: Option<PathBuf>,
    pub nfs_export: Option<String>,
}

impl StoreResolution {
    /// Read the store contract off a resource. Missing kind/path is a typed
    /// error — the adapter does not invent a third location.
    pub fn from_resource(resource: &Resource) -> Result<Self, FabricError> {
        if resource.kind != ResourceKind::Storage {
            return Err(FabricError::IncompatibleResourceKind {
                adapter: "fabric-store".into(),
                expected: ResourceKind::Storage,
                actual: resource.kind,
            });
        }
        let kind = StoreKind::parse(
            resource
                .attributes
                .get("store.kind")
                .map(String::as_str)
                .unwrap_or("bind"),
        )?;
        let path = resource
            .attributes
            .get("store.path")
            .cloned()
            .ok_or_else(|| FabricError::MissingResourceAttribute("store.path".into()))?;
        let tag = resource
            .attributes
            .get("store.tag")
            .cloned()
            .unwrap_or_else(|| "nauti".into());
        let socket = resource.attributes.get("store.socket").map(PathBuf::from);
        let nfs_export = resource.attributes.get("store.nfs_export").cloned();
        if kind == StoreKind::Nfs && nfs_export.is_none() {
            return Err(FabricError::MissingResourceAttribute("store.nfs_export".into()));
        }
        if kind == StoreKind::VirtioFs && socket.is_none() {
            return Err(FabricError::MissingResourceAttribute("store.socket".into()));
        }
        Ok(Self {
            resource_id: resource.id.clone(),
            kind,
            path: PathBuf::from(path),
            tag,
            socket,
            nfs_export,
        })
    }

    /// Cloud Hypervisor `--fs` spec for a virtio-fs store. Bind/NFS stores
    /// do not emit this; they are host-side.
    pub fn virtio_fs_spec(&self) -> Option<String> {
        let socket = self.socket.as_ref()?;
        if self.kind != StoreKind::VirtioFs {
            return None;
        }
        Some(format!("tag={},socket={}", self.tag, socket.display()))
    }

    /// The path a job on this node actually opens. NFS jobs open the
    /// export string; everyone else opens `store.path`.
    pub fn job_path(&self) -> String {
        match self.kind {
            StoreKind::Nfs => self
                .nfs_export
                .clone()
                .unwrap_or_else(|| self.path.display().to_string()),
            StoreKind::Bind | StoreKind::VirtioFs => self.path.display().to_string(),
        }
    }
}

/// Default exclusive fabric store advertised in host inventory.
pub fn default_store_resource(node: impl Into<String>) -> Resource {
    Resource {
        id: "local.store.fabric".into(),
        kind: ResourceKind::Storage,
        capacity: 1,
        unit: "store".into(),
        node: node.into(),
        state: ResourceState::Available,
        exclusive: true,
        attributes: BTreeMap::from([
            ("store.kind".into(), "bind".into()),
            ("store.path".into(), "/var/lib/nauti/store".into()),
            ("store.tag".into(), "nauti".into()),
        ]),
    }
}

/// Lease-gated store attach. Tracks live attachments so release detaches.
#[derive(Debug, Default)]
pub struct FabricStoreAdapter {
    live: Mutex<HashMap<u64, StoreResolution>>,
}

impl FabricStoreAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn live_count(&self) -> usize {
        self.live.lock().map(|m| m.len()).unwrap_or(0)
    }

    fn ensure_dir(path: &Path) -> Result<(), FabricError> {
        std::fs::create_dir_all(path).map_err(|error| FabricError::AdapterBackendUnavailable {
            adapter: "fabric-store".into(),
            reason: format!("cannot create {}: {error}", path.display()),
        })
    }
}

impl ResourceAdapter for FabricStoreAdapter {
    fn name(&self) -> &str {
        "fabric-store"
    }

    fn attach(&self, resource: &Resource, lease: &Lease) -> Result<Attachment, FabricError> {
        let resolution = StoreResolution::from_resource(resource)?;
        if resolution.kind != StoreKind::Nfs {
            Self::ensure_dir(&resolution.path)?;
        }
        if let Ok(mut live) = self.live.lock() {
            live.insert(lease.id, resolution.clone());
        }
        Ok(Attachment {
            resource_id: resource.id.clone(),
            lease_id: lease.id,
            adapter: self.name().into(),
            details: BTreeMap::from([
                ("store.kind".into(), resolution.kind.as_str().into()),
                ("store.path".into(), resolution.job_path()),
                (
                    "store.virtio_fs".into(),
                    resolution.virtio_fs_spec().unwrap_or_default(),
                ),
                ("store.tag".into(), resolution.tag),
            ]),
        })
    }

    fn detach(&self, resource: &Resource, lease: &Lease) -> Result<(), FabricError> {
        let mut live = self.live.lock().map_err(|_| FabricError::AdapterBackendUnavailable {
            adapter: "fabric-store".into(),
            reason: "store attachment map poisoned".into(),
        })?;
        match live.remove(&lease.id) {
            Some(resolution) if resolution.resource_id == resource.id => Ok(()),
            Some(other) => {
                live.insert(lease.id, other);
                Err(FabricError::LeaseNotFound)
            }
            None => Ok(()),
        }
    }

    fn capability_report(&self) -> AdapterReport {
        let virtiofsd = which::which("virtiofsd").ok();
        AdapterReport {
            name: self.name().into(),
            scope: "fabric-store".into(),
            healthy: true,
            detail: BTreeMap::from([
                ("store.bind".into(), "available".into()),
                (
                    "store.virtiofsd".into(),
                    virtiofsd
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "missing".into()),
                ),
                ("store.nfs".into(), "explicit-attr-only".into()),
            ]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Lease;

    fn store_resource(id: &str, kind: &str, path: &str) -> Resource {
        let mut attributes = BTreeMap::from([
            ("store.kind".into(), kind.into()),
            ("store.path".into(), path.into()),
            ("store.tag".into(), "nauti".into()),
        ]);
        if kind == "virtio-fs" {
            attributes.insert("store.socket".into(), format!("{path}.sock"));
        }
        if kind == "nfs" {
            attributes.insert("store.nfs_export".into(), "t440:/var/lib/nauti/store".into());
        }
        Resource {
            id: id.into(),
            kind: ResourceKind::Storage,
            capacity: 1,
            unit: "store".into(),
            node: "local".into(),
            state: ResourceState::Available,
            exclusive: true,
            attributes,
        }
    }

    fn lease(id: u64, resource_id: &str) -> Lease {
        Lease {
            id,
            resource_id: resource_id.into(),
            owner: "test".into(),
        }
    }

    #[test]
    fn bind_and_virtiofs_resolve_the_same_job_path() {
        let dir = std::env::temp_dir().join(format!("nauti-store-same-{}", std::process::id()));
        let bind = StoreResolution::from_resource(&store_resource("store.0", "bind", dir.to_str().unwrap()))
            .unwrap();
        let fs = StoreResolution::from_resource(&store_resource(
            "store.0",
            "virtio-fs",
            dir.to_str().unwrap(),
        ))
        .unwrap();
        assert_eq!(bind.job_path(), fs.job_path());
        assert_eq!(bind.resource_id, fs.resource_id);
        assert!(fs.virtio_fs_spec().unwrap().contains("tag=nauti"));
        assert!(bind.virtio_fs_spec().is_none());
    }

    #[test]
    fn nfs_requires_an_export_and_uses_it_as_the_job_path() {
        let missing = Resource {
            id: "store.nfs".into(),
            kind: ResourceKind::Storage,
            capacity: 1,
            unit: "store".into(),
            node: "c2".into(),
            state: ResourceState::Available,
            exclusive: true,
            attributes: BTreeMap::from([
                ("store.kind".into(), "nfs".into()),
                ("store.path".into(), "/mnt/nauti".into()),
            ]),
        };
        assert_eq!(
            StoreResolution::from_resource(&missing).unwrap_err(),
            FabricError::MissingResourceAttribute("store.nfs_export".into())
        );

        let ok = StoreResolution::from_resource(&store_resource("store.nfs", "nfs", "/mnt/nauti")).unwrap();
        assert_eq!(ok.job_path(), "t440:/var/lib/nauti/store");
    }

    #[test]
    fn attach_then_release_detaches_cleanly() {
        let dir = std::env::temp_dir().join(format!("nauti-store-lease-{}", std::process::id()));
        let adapter = FabricStoreAdapter::new();
        let resource = store_resource("store.0", "bind", dir.to_str().unwrap());
        let lease = lease(7, "store.0");

        let attachment = adapter.attach(&resource, &lease).unwrap();
        assert_eq!(attachment.adapter, "fabric-store");
        assert_eq!(attachment.details["store.path"], dir.display().to_string());
        assert_eq!(adapter.live_count(), 1);
        assert!(dir.is_dir());

        adapter.detach(&resource, &lease).unwrap();
        assert_eq!(adapter.live_count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn virtio_fs_without_socket_is_a_typed_error() {
        let resource = Resource {
            id: "store.fs".into(),
            kind: ResourceKind::Storage,
            capacity: 1,
            unit: "store".into(),
            node: "local".into(),
            state: ResourceState::Available,
            exclusive: true,
            attributes: BTreeMap::from([
                ("store.kind".into(), "virtio-fs".into()),
                ("store.path".into(), "/var/lib/nauti/store".into()),
            ]),
        };
        assert_eq!(
            StoreResolution::from_resource(&resource).unwrap_err(),
            FabricError::MissingResourceAttribute("store.socket".into())
        );
    }
}
