//! Cloud Hypervisor integration.
//!
//! The [`CloudHypervisorAdapter`] translates a leased `Resource` (whose attributes
//! describe the desired VM/disk layout) into a Cloud Hypervisor VM creation plus
//! virtio/vhost-user attach, and exposes an explicit [`CloudHypervisorAdapter::reconcile`]
//! step that tears down the VM after the lease is released. Idempotency, lease
//! gating, and capability reporting follow the same contracts as the
//! `local-resource` and `network-resource` adapters.
//!
//! See [CLOUD_HYPERVISOR_INTEGRATION.md](../../../CLOUD_HYPERVISOR_INTEGRATION.md)
//! and [ADAPTER_ARCHITECTURE.md](../../../ADAPTER_ARCHITECTURE.md) for the design.

mod cloud_hypervisor;

pub use cloud_hypervisor::{CloudHypervisorAdapter, Launcher, ProcessLauncher, VmResourceSpec};
