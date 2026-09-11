//! vhost-user device backends (M7 — vhost-user backend proof).
//!
//! A vhost-user backend is a userspace process that implements the device half
//! of a virtio device and speaks the vhost-user protocol over a Unix domain
//! socket. Cloud Hypervisor connects as the *frontend* (`--user-device
//! socket=...` at boot, or `ch-remote add-user-device` hotplug), shares guest
//! RAM via fd passing, and the guest then sees an ordinary virtio device.
//!
//! This module provides:
//!
//! * [`rng`] — a virtio-rng backend adapted from the rust-vmm `vhost-device`
//!   RNG template (the punchlist's mandated starting point). The entropy
//!   source is any readable file-like object: `/dev/urandom` locally, or a
//!   FIFO fed by [`entropy::entropy_pump`] for the remote variant.
//! * [`entropy`] — the remote stub: a tiny length-prefixed TCP entropy
//!   protocol (`entropy-serve` on the device host, `entropy-pump` on the VM
//!   host feeding a FIFO). This is deliberately a *stub*: the virtqueue and
//!   guest-memory fd passing stay host-local because the vhost-user protocol
//!   requires SCM_RIGHTS fd passing, which cannot cross a TCP connection.
//!   What crosses the network is the device *payload* (entropy bytes), which
//!   is the honest remote-resource boundary for this milestone.
//!
//! ## Fabric wiring
//!
//! A VM resource carries `vmm.user_device = <socket path>`; the
//! `cloud-hypervisor` adapter then emits `--user-device socket=<path>` at
//! launch (see `vmm::cloud_hypervisor`). Lease/attach/release follow the same
//! adapter contract as every other resource: the fabric tracks intent, the
//! operator (or `nauti vhost serve`) runs the backend process.
//!
//! ## Degradation contract
//!
//! If the backend process dies, Cloud Hypervisor loses the vhost-user
//! connection: the guest's virtio device stops completing requests (reads
//! stall, `dmesg` shows the device wedging) and the CH process log records
//! the disconnect. If only the *remote entropy peer* dies, the FIFO writer
//! (`entropy_pump`) exits, but because the backend holds the FIFO open
//! read-write, guest reads stall rather than complete with zero bytes —
//! a guest is never silently served empty entropy.

pub mod entropy;
pub mod rng;

pub use entropy::{entropy_pump, entropy_serve, EntropyError};
pub use rng::{serve_rng, RngConfig, RngError};
