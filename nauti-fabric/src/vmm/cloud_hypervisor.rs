//! Cloud Hypervisor reconciler adapter.
//!
//! The adapter treats the Cloud Hypervisor binary as a child process and treats
//! the leased `Resource` (of [`ResourceKind::Device`] plus a `vmm.*` attribute set)
//! as the *trigger* for creating a VM with that resource attached. The
//! [`Launcher`] trait abstracts the spawn so the idempotency and reconcile
//! contracts can be unit-tested without a real VMM on the host.
//!
//! ## Resource contract
//!
//! A `Resource` is accepted by this adapter when it carries all of:
//!
//! | Attribute          | Meaning |
//! |--------------------|---------|
//! | `vmm.binary`       | Absolute path to the `cloud-hypervisor` binary (the adapter does **not** consult `$PATH` at attach time beyond the explicit value here, so a test or operator can pin a specific build). |
//! | `vmm.api_socket`   | Filesystem path for the per-VM API socket. The adapter treats an existing socket whose peer reports `Running` as "already reconciled" and returns success without re-launching. |
//! | `vmm.kernel`       | Path to the kernel image passed via `--kernel`. |
//! | `vmm.rootfs`       | Path to the rootfs/disk image passed via `--disk path=...`. (A separate `vmm.virtio_blk` may override this.) |
//! | `vmm.vm_name`      | (Optional) VM name; defaults to the resource id. |
//! | `vmm.vcpus`        | (Optional) vCPU count; defaults to `1`. |
//! | `vmm.memory_mib`   | (Optional) memory size in MiB; defaults to `512`. |
//! | `vmm.virtio_blk`   | (Optional) extra `--disk path=...` to attach as a second virtio block device (leased virtio/vhost-user device; the rest of this proof covers virtio-blk, with vhost-user being a follow-up that requires the `vhost-user-backend` family). |
//!
//! ## Lifecycle
//!
//! 1. `Fabric::attach(adapter, lease)` calls `CloudHypervisorAdapter::attach`.
//! 2. `attach` validates attributes, checks the binary, then either no-ops (if
//!    the API socket already reports the VM as running) or calls
//!    [`Launcher::launch_vm`] with the constructed argv.
//! 3. `Fabric::release(lease)` only frees the lease; it does **not** tear the
//!    VM down. Tear-down is the operator's job and happens via
//!    [`CloudHypervisorAdapter::reconcile`], which calls
//!    [`Launcher::remove_vm`] for every managed VM. This split is deliberate
//!    (see `CLOUD_HYPERVISOR_INTEGRATION.md`): the fabric tracks intent, the
//!    operator/CLI triggers actual state convergence.
//! 4. A second `attach` for the same resource id+lease is a no-op success
//!    (idempotent reconciliation). A second `attach` for the same resource id
//!    under a *different* lease is a `ResourceAlreadyLeased` error from the
//!    `Fabric` itself, so the adapter never sees it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::{
    AdapterReport, Attachment, FabricError, Lease, Resource, ResourceAdapter, ResourceKind,
};

/// A `Launcher` shells out to Cloud Hypervisor (and `ch-remote`) on behalf of
/// the adapter. It is a trait so that tests can substitute an in-memory mock
/// that records every spawn and returns canned exit codes, proving the
/// adapter's idempotency and reconcile contracts without requiring a real VMM.
pub trait Launcher: Send + Sync {
    /// Returns the absolute path of the `cloud-hypervisor` binary the adapter
    /// will invoke, or `None` if the binary is not present on this host.
    fn locate_binary(&self, configured_path: &Path) -> Option<PathBuf>;

    /// Spawn a VM. Returns `Ok(())` if the process exits 0 within the wait
    /// window, or an error otherwise. The `argv` is the exact argument list
    /// the adapter chose; tests can assert on its contents.
    fn launch_vm(&self, binary: &Path, argv: &[String]) -> Result<(), String>;

    /// Tear a VM down (e.g. `ch-remote --api-socket <sock> shutdown-vm`).
    /// Returns `Ok(())` if the process exits 0.
    fn remove_vm(&self, api_socket: &Path, vm_name: &str) -> Result<(), String>;

    /// Returns true if the API socket is live AND the VM it represents is in
    /// the `Running` state. The default implementation just checks for the
    /// socket's existence; tests override it to return deterministic values.
    fn vm_is_running(&self, api_socket: &Path) -> bool {
        api_socket.exists()
    }
}

/// The default [`Launcher`]: shells out to a real `cloud-hypervisor` binary
/// and a real `ch-remote` binary. Construction is cheap; the `locate_binary`
/// and `vm_is_running` checks are what make the adapter's `capability_report`
/// truthful about the host environment.
#[derive(Debug, Default)]
pub struct ProcessLauncher;

impl Launcher for ProcessLauncher {
    fn locate_binary(&self, configured_path: &Path) -> Option<PathBuf> {
        if configured_path.as_os_str().is_empty() {
            return None;
        }
        if configured_path.exists() {
            Some(configured_path.to_path_buf())
        } else {
            None
        }
    }

    fn launch_vm(&self, binary: &Path, argv: &[String]) -> Result<(), String> {
        use std::process::Command;
        let status = Command::new(binary)
            .args(argv)
            .status()
            .map_err(|error| format!("failed to spawn {}: {error}", binary.display()))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!(
                "{} exited with status {status}",
                binary.display()
            ))
        }
    }

    fn remove_vm(&self, api_socket: &Path, vm_name: &str) -> Result<(), String> {
        use std::process::Command;
        let status = Command::new("ch-remote")
            .arg(format!("--api-socket={}", api_socket.display()))
            .arg("remove-vm")
            .arg(vm_name)
            .status()
            .map_err(|error| format!("failed to spawn ch-remote: {error}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!(
                "ch-remote remove-vm {vm_name} exited with status {status}"
            ))
        }
    }
}

/// The internal record of a VM this adapter has launched, so a second
/// `attach` for the same resource+lease is a no-op and `reconcile` knows
/// what to remove.
#[derive(Clone, Debug)]
struct ManagedVm {
    resource_id: String,
    api_socket: PathBuf,
    vm_name: String,
}

/// Translates a leased `Resource` (with `vmm.*` attributes) into a Cloud
/// Hypervisor VM and a virtio-blk attachment.
///
/// The adapter is `Send + Sync` and is intended to be shared via
/// `Arc<CloudHypervisorAdapter<...>>` exactly like `LocalResourceAdapter`.
pub struct CloudHypervisorAdapter<L: Launcher> {
    binary: PathBuf,
    launcher: Arc<L>,
    managed: Mutex<HashMap<String, ManagedVm>>,
}

impl<L: Launcher> std::fmt::Debug for CloudHypervisorAdapter<L> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CloudHypervisorAdapter")
            .field("binary", &self.binary)
            .field("managed_count", &self.managed.lock().map(|m| m.len()).unwrap_or(0))
            .finish()
    }
}

impl CloudHypervisorAdapter<ProcessLauncher> {
    /// Build an adapter pinned to a specific `cloud-hypervisor` binary path.
    /// The path is checked for existence here only as a hint for
    /// `capability_report`; the attach path re-checks at call time so a binary
    /// installed between construction and attach is honored.
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self::with_launcher(binary.into(), Arc::new(ProcessLauncher))
    }
}

impl<L: Launcher> CloudHypervisorAdapter<L> {
    /// Build an adapter with a custom [`Launcher`]. Used by tests to inject a
    /// recording mock; the production constructor is [`Self::new`].
    pub fn with_launcher(binary: PathBuf, launcher: Arc<L>) -> Self {
        Self {
            binary,
            launcher,
            managed: Mutex::new(HashMap::new()),
        }
    }

    /// Build the argv the adapter would pass to `cloud-hypervisor` for a given
    /// resource, **without** actually spawning anything. Public so tests can
    /// assert on the exact flags the adapter chooses, and so the
    /// `nauti vm launch` CLI can dry-run a planned launch.
    pub fn build_argv(&self, resource: &Resource) -> Result<Vec<String>, FabricError> {
        let attrs = &resource.attributes;
        let api_socket = require_attr(attrs, "vmm.api_socket")?;
        let kernel = require_attr(attrs, "vmm.kernel")?;
        let rootfs = require_attr(attrs, "vmm.rootfs")?;
        let vm_name = attrs
            .get("vmm.vm_name")
            .cloned()
            .unwrap_or_else(|| resource.id.clone());
        let vcpus = attrs
            .get("vmm.vcpus")
            .cloned()
            .unwrap_or_else(|| "1".into());
        let memory_mib = attrs
            .get("vmm.memory_mib")
            .cloned()
            .unwrap_or_else(|| "512".into());

        let mut argv: Vec<String> = vec![
            "--api-socket".into(),
            format!("socket={api_socket}"),
            "--cpus".into(),
            format!("boot={vcpus}"),
            "--memory".into(),
            format!("size={memory_mib}"),
            "--kernel".into(),
            kernel,
            "--disk".into(),
            format!("path={rootfs}"),
            "--serial".into(),
            "tty".into(),
            "--console".into(),
            "off".into(),
            "--boot".into(),
            "kernel".into(),
            "--name".into(),
            vm_name,
        ];
        if let Some(extra_disk) = attrs.get("vmm.virtio_blk") {
            argv.push("--disk".into());
            argv.push(format!("path={extra_disk}"));
        }
        Ok(argv)
    }

    /// Tear down every VM this adapter has launched since the last reconcile.
    /// Returns the per-VM outcome; callers can log or surface failures.
    pub fn reconcile(&self) -> Vec<Result<String, String>> {
        let managed = match self.managed.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let entries: Vec<ManagedVm> = managed.values().cloned().collect();
        drop(managed);

        let mut results = Vec::with_capacity(entries.len());
        for vm in entries {
            let outcome = self
                .launcher
                .remove_vm(&vm.api_socket, &vm.vm_name)
                .map(|_| vm.resource_id.clone());
            if outcome.is_ok() {
                if let Ok(mut managed) = self.managed.lock() {
                    managed.remove(&vm.resource_id);
                }
            }
            results.push(outcome);
        }
        results
    }

    #[cfg(test)]
    fn managed_count(&self) -> usize {
        self.managed.lock().map(|m| m.len()).unwrap_or(0)
    }
}

impl<L: Launcher + 'static> ResourceAdapter for CloudHypervisorAdapter<L> {
    fn name(&self) -> &str {
        "cloud-hypervisor"
    }

    fn attach(&self, resource: &Resource, lease: &Lease) -> Result<Attachment, FabricError> {
        if resource.kind != ResourceKind::Device {
            return Err(FabricError::IncompatibleResourceKind {
                adapter: self.name().into(),
                expected: ResourceKind::Device,
                actual: resource.kind,
            });
        }
        let binary = match self.launcher.locate_binary(&self.binary) {
            Some(path) => path,
            None => {
                return Err(FabricError::AdapterBackendUnavailable {
                    adapter: self.name().into(),
                    reason: format!(
                        "cloud-hypervisor binary not found at {}",
                        self.binary.display()
                    ),
                });
            }
        };
        let api_socket = PathBuf::from(require_attr(&resource.attributes, "vmm.api_socket")?);
        let vm_name = resource
            .attributes
            .get("vmm.vm_name")
            .cloned()
            .unwrap_or_else(|| resource.id.clone());

        // Idempotency: if we already manage this resource, this is a re-attach
        // of the same lease, and we return success without launching again.
        if let Ok(managed) = self.managed.lock() {
            if let Some(existing) = managed.get(&resource.id) {
                if existing.vm_name == vm_name {
                    return Ok(Attachment {
                        resource_id: resource.id.clone(),
                        lease_id: lease.id,
                        adapter: self.name().into(),
                        details: detail_map(&binary, &api_socket, &vm_name, true),
                    });
                }
            }
        }

        // Also idempotent against an externally-running VM whose API socket
        // exists: the operator may have launched it themselves, in which case
        // our attach becomes a no-op success.
        if self.launcher.vm_is_running(&api_socket) {
            self.record_managed(resource, api_socket.clone(), vm_name.clone());
            return Ok(Attachment {
                resource_id: resource.id.clone(),
                lease_id: lease.id,
                adapter: self.name().into(),
                details: detail_map(&binary, &api_socket, &vm_name, true),
            });
        }

        let argv = self.build_argv(resource)?;
        self.launcher
            .launch_vm(&binary, &argv)
            .map_err(|reason| FabricError::AdapterBackendUnavailable {
                adapter: self.name().into(),
                reason,
            })?;
        self.record_managed(resource, api_socket.clone(), vm_name.clone());

        Ok(Attachment {
            resource_id: resource.id.clone(),
            lease_id: lease.id,
            adapter: self.name().into(),
            details: detail_map(&binary, &api_socket, &vm_name, false),
        })
    }

    fn capability_report(&self) -> AdapterReport {
        let healthy = self.launcher.locate_binary(&self.binary).is_some();
        let detail: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::from([
            ("vmm.binary".into(), self.binary.display().to_string()),
            ("launcher".into(), std::any::type_name::<L>().into()),
        ]);
        AdapterReport {
            name: self.name().into(),
            scope: "vmm-runtime".into(),
            healthy,
            detail,
        }
    }
}

impl<L: Launcher> CloudHypervisorAdapter<L> {
    fn record_managed(&self, resource: &Resource, api_socket: PathBuf, vm_name: String) {
        if let Ok(mut managed) = self.managed.lock() {
            managed.insert(
                resource.id.clone(),
                ManagedVm {
                    resource_id: resource.id.clone(),
                    api_socket,
                    vm_name,
                },
            );
        }
    }
}

fn require_attr(
    attrs: &std::collections::BTreeMap<String, String>,
    key: &str,
) -> Result<String, FabricError> {
    attrs
        .get(key)
        .cloned()
        .ok_or_else(|| FabricError::MissingResourceAttribute(key.into()))
}

fn detail_map(
    binary: &Path,
    api_socket: &Path,
    vm_name: &str,
    already_running: bool,
) -> std::collections::BTreeMap<String, String> {
    std::collections::BTreeMap::from([
        ("vmm.binary".into(), binary.display().to_string()),
        ("vmm.api_socket".into(), api_socket.display().to_string()),
        ("vmm.vm_name".into(), vm_name.into()),
        (
            "vmm.launched_by_adapter".into(),
            (!already_running).to_string(),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use crate::{ResourceKind, ResourceState};

    fn device_resource(id: &str) -> Resource {
        Resource {
            id: id.into(),
            kind: ResourceKind::Device,
            capacity: 1,
            unit: "vm".into(),
            node: "local".into(),
            state: ResourceState::Available,
            exclusive: true,
            attributes: BTreeMap::from([
                ("vmm.binary".into(), "/does/not/exist/cloud-hypervisor".into()),
                ("vmm.api_socket".into(), format!("/tmp/nauti-test-{id}.sock")),
                ("vmm.kernel".into(), "/tmp/kernel".into()),
                ("vmm.rootfs".into(), "/tmp/rootfs".into()),
            ]),
        }
    }

    fn lease(resource_id: &str) -> Lease {
        Lease { id: 1, resource_id: resource_id.into(), owner: "test".into() }
    }

    /// Recording mock: captures every `launch_vm` and `remove_vm` call so the
    /// idempotency and reconcile contracts can be asserted on. `vm_is_running`
    /// is a per-socket flag the test toggles to simulate "operator pre-launched
    /// it" for a specific VM.
    #[derive(Debug)]
    struct MockLauncher {
        launches: Mutex<Vec<(PathBuf, Vec<String>)>>,
        removes: Mutex<Vec<(PathBuf, String)>>,
        binary_present: bool,
        running: Mutex<std::collections::BTreeMap<PathBuf, bool>>,
    }

    impl MockLauncher {
        fn new(binary_present: bool) -> Self {
            Self {
                launches: Mutex::new(Vec::new()),
                removes: Mutex::new(Vec::new()),
                binary_present,
                running: Mutex::new(std::collections::BTreeMap::new()),
            }
        }
        fn launches(&self) -> Vec<(PathBuf, Vec<String>)> {
            self.launches.lock().unwrap().clone()
        }
        fn removes(&self) -> Vec<(PathBuf, String)> {
            self.removes.lock().unwrap().clone()
        }
        fn set_running(&self, socket: &Path, running: bool) {
            self.running.lock().unwrap().insert(socket.to_path_buf(), running);
        }
    }

    impl Launcher for MockLauncher {
        fn locate_binary(&self, _configured_path: &Path) -> Option<PathBuf> {
            if self.binary_present {
                Some(PathBuf::from("/mock/cloud-hypervisor"))
            } else {
                None
            }
        }
        fn launch_vm(&self, binary: &Path, argv: &[String]) -> Result<(), String> {
            // Extract the API socket from the argv (always the second token:
            // --api-socket socket=<path>).
            let socket = argv
                .iter()
                .find_map(|arg| arg.strip_prefix("socket="))
                .map(PathBuf::from);
            self.launches
                .lock()
                .unwrap()
                .push((binary.to_path_buf(), argv.to_vec()));
            if let Some(socket) = socket {
                self.running.lock().unwrap().insert(socket, true);
            }
            Ok(())
        }
        fn remove_vm(&self, api_socket: &Path, vm_name: &str) -> Result<(), String> {
            self.removes
                .lock()
                .unwrap()
                .push((api_socket.to_path_buf(), vm_name.into()));
            self.running.lock().unwrap().insert(api_socket.to_path_buf(), false);
            Ok(())
        }
        fn vm_is_running(&self, api_socket: &Path) -> bool {
            self.running
                .lock()
                .unwrap()
                .get(api_socket)
                .copied()
                .unwrap_or(false)
        }
    }

    #[test]
    fn adapter_rejects_non_device_kind_resource() {
        let launcher = Arc::new(MockLauncher::new(true));
        let adapter = CloudHypervisorAdapter::with_launcher(
            PathBuf::from("/mock/cloud-hypervisor"),
            launcher,
        );
        let mut resource = device_resource("vm.0");
        resource.kind = ResourceKind::Storage;
        let error = adapter.attach(&resource, &lease("vm.0")).unwrap_err();
        assert_eq!(
            error,
            FabricError::IncompatibleResourceKind {
                adapter: "cloud-hypervisor".into(),
                expected: ResourceKind::Device,
                actual: ResourceKind::Storage,
            }
        );
    }

    #[test]
    fn adapter_requires_vmm_attributes() {
        let launcher = Arc::new(MockLauncher::new(true));
        let adapter = CloudHypervisorAdapter::with_launcher(
            PathBuf::from("/mock/cloud-hypervisor"),
            launcher,
        );
        let mut resource = device_resource("vm.0");
        resource.attributes.remove("vmm.kernel");
        let error = adapter.attach(&resource, &lease("vm.0")).unwrap_err();
        assert_eq!(
            error,
            FabricError::MissingResourceAttribute("vmm.kernel".into())
        );
    }

    #[test]
    fn attach_rejects_when_binary_is_missing() {
        let launcher = Arc::new(MockLauncher::new(false));
        let adapter = CloudHypervisorAdapter::with_launcher(
            PathBuf::from("/mock/cloud-hypervisor"),
            launcher,
        );
        let error = adapter
            .attach(&device_resource("vm.0"), &lease("vm.0"))
            .unwrap_err();
        assert!(matches!(
            error,
            FabricError::AdapterBackendUnavailable { ref adapter, .. } if adapter == "cloud-hypervisor"
        ));
    }

    #[test]
    fn capability_report_is_unhealthy_when_binary_missing() {
        let launcher = Arc::new(MockLauncher::new(false));
        let adapter = CloudHypervisorAdapter::with_launcher(
            PathBuf::from("/mock/cloud-hypervisor"),
            launcher,
        );
        let report = adapter.capability_report();
        assert_eq!(report.name, "cloud-hypervisor");
        assert_eq!(report.scope, "vmm-runtime");
        assert!(!report.healthy);
        assert!(report.detail.contains_key("vmm.binary"));
    }

    #[test]
    fn capability_report_is_healthy_when_binary_present() {
        let launcher = Arc::new(MockLauncher::new(true));
        let adapter = CloudHypervisorAdapter::with_launcher(
            PathBuf::from("/mock/cloud-hypervisor"),
            launcher,
        );
        let report = adapter.capability_report();
        assert!(report.healthy);
    }

    #[test]
    fn attach_launches_vm_on_first_call_and_no_ops_on_repeat() {
        let launcher = Arc::new(MockLauncher::new(true));
        let adapter = CloudHypervisorAdapter::with_launcher(
            PathBuf::from("/mock/cloud-hypervisor"),
            Arc::clone(&launcher),
        );
        let resource = device_resource("vm.0");

        let first = adapter.attach(&resource, &lease("vm.0")).unwrap();
        let second = adapter.attach(&resource, &lease("vm.0")).unwrap();

        assert_eq!(launcher.launches().len(), 1, "second attach must be a no-op");
        assert_eq!(adapter.managed_count(), 1);
        assert_eq!(first.adapter, "cloud-hypervisor");
        assert_eq!(second.adapter, "cloud-hypervisor");
        assert_eq!(first.details["vmm.launched_by_adapter"], "true");
        assert_eq!(second.details["vmm.launched_by_adapter"], "false");
    }

    #[test]
    fn attach_no_ops_when_api_socket_already_reports_running() {
        // Simulate "operator pre-launched the VM" by seeding the mock to
        // report vm_is_running() == true for this specific socket.
        let launcher = Arc::new(MockLauncher::new(true));
        launcher.set_running(&PathBuf::from("/tmp/nauti-test-vm.0.sock"), true);
        let adapter = CloudHypervisorAdapter::with_launcher(
            PathBuf::from("/mock/cloud-hypervisor"),
            Arc::clone(&launcher),
        );
        let attachment = adapter.attach(&device_resource("vm.0"), &lease("vm.0")).unwrap();
        assert!(launcher.launches().is_empty(), "must not re-launch a live VM");
        assert_eq!(attachment.details["vmm.launched_by_adapter"], "false");
    }

    #[test]
    fn reconcile_after_release_issues_exactly_one_remove_per_managed_vm() {
        let launcher = Arc::new(MockLauncher::new(true));
        let adapter = CloudHypervisorAdapter::with_launcher(
            PathBuf::from("/mock/cloud-hypervisor"),
            Arc::clone(&launcher),
        );
        adapter.attach(&device_resource("vm.0"), &lease("vm.0")).unwrap();
        adapter.attach(&device_resource("vm.1"), &lease("vm.1")).unwrap();
        assert_eq!(adapter.managed_count(), 2);
        assert!(launcher.launches().len() == 2);

        let outcomes = adapter.reconcile();
        assert_eq!(outcomes.len(), 2);
        for outcome in outcomes {
            assert!(outcome.is_ok(), "each managed VM should reconcile cleanly");
        }
        assert_eq!(launcher.removes().len(), 2);
        assert_eq!(adapter.managed_count(), 0);

        // A second reconcile on an empty adapter is a no-op.
        let second = adapter.reconcile();
        assert!(second.is_empty());
    }

    #[test]
    fn build_argv_carries_required_flags_and_overrides() {
        let launcher = Arc::new(MockLauncher::new(true));
        let adapter = CloudHypervisorAdapter::with_launcher(
            PathBuf::from("/mock/cloud-hypervisor"),
            launcher,
        );
        let mut resource = device_resource("vm.0");
        resource.attributes.insert("vmm.vcpus".into(), "4".into());
        resource.attributes.insert("vmm.memory_mib".into(), "2048".into());
        resource.attributes.insert("vmm.virtio_blk".into(), "/tmp/extra.raw".into());
        let argv = adapter.build_argv(&resource).unwrap();
        let joined = argv.join(" ");
        assert!(joined.contains("--api-socket socket=/tmp/nauti-test-vm.0.sock"));
        assert!(joined.contains("--cpus boot=4"));
        assert!(joined.contains("--memory size=2048"));
        assert!(joined.contains("--kernel /tmp/kernel"));
        assert!(joined.contains("--disk path=/tmp/rootfs"));
        assert!(joined.contains("--disk path=/tmp/extra.raw"));
        assert!(joined.contains("--name vm.0"));
    }
}

// Re-export the manifest so crate users can build a `Resource` with a stable,
// machine-readable attribute set rather than hand-rolling keys.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VmResourceSpec {
    pub api_socket: String,
    pub kernel: String,
    pub rootfs: String,
    pub vm_name: Option<String>,
    pub vcpus: Option<String>,
    pub memory_mib: Option<String>,
    pub virtio_blk: Option<String>,
}

impl VmResourceSpec {
    pub fn into_attributes(self) -> std::collections::BTreeMap<String, String> {
        let mut attrs = std::collections::BTreeMap::new();
        attrs.insert("vmm.api_socket".into(), self.api_socket);
        attrs.insert("vmm.kernel".into(), self.kernel);
        attrs.insert("vmm.rootfs".into(), self.rootfs);
        if let Some(vm_name) = self.vm_name {
            attrs.insert("vmm.vm_name".into(), vm_name);
        }
        if let Some(vcpus) = self.vcpus {
            attrs.insert("vmm.vcpus".into(), vcpus);
        }
        if let Some(memory_mib) = self.memory_mib {
            attrs.insert("vmm.memory_mib".into(), memory_mib);
        }
        if let Some(virtio_blk) = self.virtio_blk {
            attrs.insert("vmm.virtio_blk".into(), virtio_blk);
        }
        attrs
    }
}
