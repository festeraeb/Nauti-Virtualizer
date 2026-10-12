use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use nauti_fabric::rpc::{AgentClient, RpcRequest, RpcResponse};
#[cfg(feature = "cloud-hypervisor")]
use nauti_fabric::vmm::{CloudHypervisorAdapter, VmResourceSpec};
#[cfg(any(feature = "cloud-hypervisor", feature = "vhost-user"))]
use std::path::PathBuf;
use nauti_fabric::{Fabric, Lease, LocalProofAdapter, LocalResourceAdapter, NetworkResourceAdapter, Resource, ResourceKind, ResourceRequest, ResourceState};
#[cfg(feature = "cloud-hypervisor")]
use nauti_fabric::ResourceAdapter;

#[derive(serde::Serialize)]
struct ToolInfo {
    name: &'static str,
    purpose: &'static str,
}

#[derive(Parser)]
#[command(about = "Forge resource-fabric command line")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List the current Forge-facing tools and adapter planes.
    Tools {
        /// Emit the tool list as JSON.
        #[arg(long)]
        json: bool,
    },
    /// List adapter names and their current scope.
    Adapters {
        /// Emit the adapter list as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Discover resources available on this host.
    Inventory {
        /// Emit the resource model as JSON for a tool-calling model or automation.
        #[arg(long)]
        json: bool,
        #[arg(long, default_value = "local")]
        node: String,
        /// Re-discover local resources (host inventory + NVML GPUs when
        /// the `nvidia` feature is on) and diff against the current set
        /// before listing. Useful after a hot-swap of a GPU or any other
        /// device: the next refresh reflects whatever the kernel and
        /// NVML currently report, no agent restart required.
        #[arg(long)]
        refresh: bool,
    },
    /// Run the complete single-host resource lifecycle proof.
    Demo,
    /// Serve the fabric agent RPC protocol over authenticated Iroh/QUIC.
    AgentServe {
        #[arg(long, default_value = "local")]
        node: String,
    },
    /// Connect to a remote fabric agent and run the two-process lease/attach/release proof.
    AgentConnect {
        /// JSON-encoded `EndpointAddr` printed by `agent-serve`.
        addr: String,
    },
    /// Drive a remote fabric agent over the RPC contract — the verbs an
    /// LLM-facing tool layer (MCP) calls. `addr` is the JSON-encoded
    /// `EndpointAddr` the agent printed at startup and self-registered to
    /// its address file (default /var/lib/nauti/<node>-addr.json).
    Fabric {
        #[command(subcommand)]
        action: FabricAction,
    },
    /// Probe local NUMA/PCI topology via hwloc (requires the `numa` build feature).
    #[cfg(feature = "numa")]
    Topology {
        /// Emit the topology report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Probe local GPUs via the all-smi adapter (any brand: NVIDIA, AMD,
    /// Intel, anything the kernel drives through `/sys/class/drm`). Works
    /// without the `nvidia` build feature. NVIDIA cards are enriched with
    /// NVML telemetry when the `nvidia` feature is enabled.
    Gpus {
        /// Emit the GPU report as JSON.
        #[arg(long)]
        json: bool,
        /// Group devices by vendor (NVIDIA / AMD / Intel / ...) and list
        /// individuals within each group.
        #[arg(long)]
        grouped: bool,
        /// Show only one vendor group (e.g. `--type amd`). Case-insensitive.
        #[arg(long)]
        type_: Option<String>,
    },
    /// Manage local VMs through the Cloud Hypervisor adapter (requires the
    /// `cloud-hypervisor` build feature and the `cloud-hypervisor` + `ch-remote`
    /// binaries on PATH).
    #[cfg(feature = "cloud-hypervisor")]
    Vm {
        #[command(subcommand)]
        action: VmAction,
    },
    /// Serve vhost-user device backends and wire the remote entropy stub
    /// (requires the `vhost-user` build feature). The backend socket is
    /// attached to a VM via `nauti vm launch --user-device ...`.
    #[cfg(feature = "vhost-user")]
    Vhost {
        #[command(subcommand)]
        action: VhostAction,
    },
    /// M10 remote GPU job. `--via tailscale` refuses LAN addresses.
    Wgpu {
        #[command(subcommand)]
        action: WgpuAction,
    },
    /// CXL/DAX/famfs/DRust research plane: discover and refuse live attach.
    Research {
        #[command(subcommand)]
        action: ResearchAction,
    },
    /// One-fleet view: every agent address file in the addr dir, fabric RPC
    /// latency from this node, per-path ICMP latency, rolled-up resources.
    Fleet {
        #[arg(long)]
        json: bool,
        /// RPC ping samples per node.
        #[arg(long, default_value_t = 10)]
        samples: u32,
        /// Also time each direct IP path (LAN, 25G/RDMA, Tailscale) with ICMP.
        #[arg(long)]
        paths: bool,
        /// Write the view here (default <addr dir>/fleet-view-<this node>.json).
        #[arg(long)]
        out: Option<String>,
    },
    /// M11 head planner endpoint: ask the fabric head for a plan FRAGMENT for
    /// one role, or run the same decision order offline against a committed
    /// snapshot. One ask, one fragment; an unsatisfiable role is a typed
    /// `403 escalate-nowhere`, never an empty plan.
    Plan {
        #[command(subcommand)]
        action: PlanAction,
    },
}

#[derive(Subcommand)]
enum PlanAction {
    /// POST /v1/plan to the head and print the fragment (or the typed reject).
    Ask {
        /// Role to plan for, e.g. `coder`, `thinker`, `reviewer`.
        #[arg(long)]
        role: String,
        /// Head base URL, e.g. `http://192.168.4.40:8080` (default port 8080).
        /// Falls back to `$NAUTI_PLAN_HEAD` when omitted.
        #[arg(long)]
        head: Option<String>,
        /// Asking node's host_key (default: this host's short name).
        #[arg(long)]
        node: Option<String>,
        /// Socket timeout in seconds (default 10).
        #[arg(long, default_value_t = 10)]
        timeout_secs: u64,
        /// Emit the fragment (or the reject) as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Run the head's decision order locally against snapshot JSON. No network,
    /// no head: this is the agent check on a box where the head is unreachable.
    Offline {
        /// Role to plan for.
        #[arg(long)]
        role: String,
        /// Snapshot JSON: a host->slots map (envelope `nodes`/`hosts`/`fleet`
        /// or bare), as `fleet/planner.py` reads it.
        #[arg(long)]
        view: String,
        /// Asking node's host_key (default: this host's short name).
        #[arg(long)]
        node: Option<String>,
        /// Roles the head knows; a `FALLBACK_CHAIN`-shaped JSON file or a
        /// comma-separated list. A role outside it is a typed 404.
        #[arg(long)]
        roles: String,
        /// `nauti fleet` latency matrix for the FLEET-scope scoring term
        /// (`/var/lib/nauti/fleet-matrix.json`). Optional: without it the plan
        /// scores on headroom alone and says so.
        #[arg(long)]
        matrix: Option<String>,
        /// Planner kind: `monolithic` (fleet-wide) or `federated` (this node).
        #[arg(long, default_value = "monolithic")]
        planner: String,
        /// Emit the fragment (or the reject) as JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum WgpuAction {
    /// Serve the wgpu-remote add kernel on a Tailscale bind address.
    Serve {
        #[arg(long)]
        bind: String,
    },
    /// Submit the correctness-bar job to a Tailscale peer.
    Run {
        #[arg(long)]
        connect: String,
        #[arg(long, default_value_t = nauti_fabric::gpu::wgpu_remote::WORKLOAD_N)]
        n: usize,
    },
}

#[derive(Subcommand)]
enum ResearchAction {
    /// Walk CXL/DAX/famfs/DRust and print the typed report (never claimed).
    Discover {
        #[arg(long)]
        json: bool,
    },
}

/// M7 vhost-user subcommands. `serve` is the device backend (blocks until
/// Cloud Hypervisor disconnects); `entropy-serve` and `pump` are the two
/// halves of the remote entropy stub.
#[cfg(feature = "vhost-user")]
#[derive(Subcommand)]
enum VhostAction {
    /// Serve a virtio-rng vhost-user backend on `--socket`, blocking until
    /// the frontend (Cloud Hypervisor) disconnects.
    Serve {
        /// Unix socket path for the backend (VM attaches with
        /// `--user-device socket=<path>`).
        #[arg(long)]
        socket: PathBuf,
        /// Entropy source: `/dev/urandom` (local) or a FIFO fed by
        /// `nauti vhost pump` (remote stub).
        #[arg(long, default_value = "/dev/urandom")]
        source: PathBuf,
        /// Rate-limit window in ms (QEMU-compatible max 65536).
        #[arg(long, default_value_t = nauti_fabric::vhost::rng::MAX_PERIOD_MS)]
        period_ms: u128,
        /// Max bytes served per period (default unlimited).
        #[arg(long, default_value_t = usize::MAX)]
        max_bytes: usize,
    },
    /// Run the remote entropy server (the device-host half of the stub).
    /// Answers length-prefixed TCP requests with bytes from /dev/urandom.
    EntropyServe {
        /// Listen address, e.g. `0.0.0.0:7877`.
        #[arg(long)]
        listen: String,
    },
    /// Pump entropy from a remote `entropy-serve` into a local FIFO the
    /// RNG backend reads. Runs until the remote peer dies.
    Pump {
        /// Remote server address, e.g. `10.55.0.1:7877`.
        #[arg(long)]
        connect: String,
        /// Output path — a FIFO (created if missing) that
        /// `nauti vhost serve --source <path>` consumes.
        #[arg(long)]
        out: PathBuf,
        /// Bytes per request (default 64 KiB).
        #[arg(long, default_value_t = nauti_fabric::vhost::entropy::DEFAULT_CHUNK)]
        chunk: u32,
    },
}

#[cfg(feature = "cloud-hypervisor")]
#[derive(Subcommand)]
enum VmAction {
    /// Print the capability/health report of the cloud-hypervisor adapter.
    Capability,
    /// Lease a device-resource (the trigger for a VM) and attach it, which
    /// boots a Cloud Hypervisor VM with the requested virtio-blk layout.
    /// Idempotent: a second launch for the same resource id is a no-op.
    Launch {
        /// Stable id for the VM resource (e.g. `vm.demo.0`).
        #[arg(long)]
        resource_id: String,
        /// Absolute path to the `cloud-hypervisor` binary.
        #[arg(long)]
        binary: PathBuf,
        /// Path to the API socket the VM will listen on.
        #[arg(long)]
        api_socket: PathBuf,
        /// Path to the kernel image.
        #[arg(long)]
        kernel: PathBuf,
        /// Path to the rootfs image.
        #[arg(long)]
        rootfs: PathBuf,
        /// Optional VM name (defaults to the resource id).
        #[arg(long)]
        vm_name: Option<String>,
        /// vCPU count (default 1).
        #[arg(long, default_value = "1")]
        vcpus: String,
        /// Memory in MiB (default 512).
        #[arg(long, default_value = "512")]
        memory_mib: String,
        /// Optional second virtio-blk disk (leased virtio/vhost-user device).
        #[arg(long)]
        virtio_blk: Option<PathBuf>,
        /// Optional Cloud Hypervisor net spec, e.g. `tap=tap0,mac=de:ad:be:ef:00:01`.
        /// The named tap is provisioned (`ip tuntap add`, link up) before spawn
        /// and passed to the VM as `--net`. Requires root/CAP_NET_ADMIN.
        #[arg(long)]
        net: Option<String>,
        /// Optional vhost-user backend socket to attach at boot via
        /// `--user-device socket=...` (M7; serve one with `nauti vhost serve`).
        #[arg(long)]
        user_device: Option<PathBuf>,
        /// virtio-fs spec, e.g. `tag=nauti,socket=/tmp/virtiofs.sock` (M8).
        #[arg(long)]
        fs: Option<String>,
        /// vsock spec, e.g. `cid=3,socket=/tmp/nauti-vsock.sock` (M9).
        #[arg(long)]
        vsock: Option<String>,
        /// Enable Cloud Hypervisor virtio-watchdog.
        #[arg(long, default_value_t = false)]
        watchdog: bool,
        /// Guest kernel cmdline.
        #[arg(long)]
        cmdline: Option<String>,
        /// Optional initramfs path.
        #[arg(long)]
        initramfs: Option<PathBuf>,
        /// Builtin virtio-rng source (default /dev/urandom). Named id=ch-builtin-rng.
        #[arg(long)]
        rng_src: Option<String>,
        /// Force guest memory shared=on.
        #[arg(long, default_value_t = false)]
        shared: bool,
        /// Lease TTL in seconds (default 30).
        #[arg(long, default_value = "30")]
        ttl_secs: u64,
    },
    /// Tear down every VM this process has launched via the adapter.
    Reconcile,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().command {
        Command::Tools { json } => tools(json),
        Command::Adapters { json } => adapters(json),
        Command::Inventory { json, node, refresh } => inventory(&node, json, refresh),
        Command::Demo => demo(),
        Command::Fleet { json, samples, paths, out } => fleet(json, samples, paths, out),
        Command::Plan { action } => plan_action(action),
        Command::AgentServe { node } => agent_serve(&node),
        Command::AgentConnect { addr } => agent_connect(&addr),
        Command::Fabric { action } => fabric_action(action),
        #[cfg(feature = "numa")]
        Command::Topology { json } => topology(json),
        Command::Gpus { json, grouped, type_ } => gpus(json, grouped, type_),
        #[cfg(feature = "cloud-hypervisor")]
        Command::Vm { action } => match action {
            VmAction::Capability => vm_capability(),
            VmAction::Launch {
                resource_id,
                binary,
                api_socket,
                kernel,
                rootfs,
                vm_name,
                vcpus,
                memory_mib,
                virtio_blk,
                net,
                user_device,
                fs,
                vsock,
                watchdog,
                cmdline,
                initramfs,
                rng_src,
                shared,
                ttl_secs,
            } => vm_launch(VmLaunchArgs {
                resource_id,
                binary,
                api_socket,
                kernel,
                rootfs,
                vm_name,
                vcpus,
                memory_mib,
                virtio_blk,
                net,
                user_device,
                fs,
                vsock,
                watchdog,
                cmdline,
                initramfs,
                rng_src,
                shared,
                ttl_secs,
            }),
            VmAction::Reconcile => vm_reconcile(),
        },
        #[cfg(feature = "vhost-user")]
        Command::Vhost { action } => match action {
            VhostAction::Serve { socket, source, period_ms, max_bytes } => vhost_serve(
                socket,
                source,
                period_ms,
                max_bytes,
            ),
            VhostAction::EntropyServe { listen } => vhost_entropy_serve(&listen),
            VhostAction::Pump { connect, out, chunk } => vhost_pump(&connect, &out, chunk),
        },
        Command::Wgpu { action } => match action {
            WgpuAction::Serve { bind } => wgpu_serve(&bind),
            WgpuAction::Run { connect, n } => wgpu_run(&connect, n),
        },
        Command::Research { action } => match action {
            ResearchAction::Discover { json } => research_discover(json),
        },
    }
}

fn tools(json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let tools = vec![
        ToolInfo { name: "inventory", purpose: "discover host resources" },
        ToolInfo { name: "adapters", purpose: "list local and network adapter planes" },
        ToolInfo { name: "demo", purpose: "prove lease, attach, and release" },
    ];

    if json {
        println!("{}", serde_json::to_string_pretty(&tools)?);
    } else {
        println!("NAME\tPURPOSE");
        for tool in tools {
            println!("{}\t{}", tool.name, tool.purpose);
        }
    }

    Ok(())
}

fn adapters(json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let fabric = Fabric::default();
    fabric.register_adapter(Arc::new(LocalProofAdapter));
    fabric.register_adapter(Arc::new(LocalResourceAdapter));
    fabric.register_adapter(Arc::new(NetworkResourceAdapter));
    let lemonade = nauti_fabric::LemonadeAdapter::new(nauti_fabric::LemonadeConfig {
        host: std::env::var("LEMONADE_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
        port: std::env::var("LEMONADE_PORT")
            .map(|p| p.parse().unwrap_or(13305))
            .unwrap_or(13305),
        api_key: std::env::var("LEMONADE_API_KEY").ok(),
    });
    fabric.register_adapter(Arc::new(lemonade));
    fabric.register_adapter(Arc::new(nauti_fabric::FabricStoreAdapter::new()));
    fabric.register_adapter(Arc::new(nauti_fabric::WgpuRemoteAdapter));
    fabric.register_adapter(Arc::new(nauti_fabric::ResearchMemoryAdapter));
    fabric.register_adapter(Arc::new(nauti_fabric::hwinfo::RdmaAdapter::default()));
    // VfioGpuAdapter lives in `nauti_fabric::vmm`, and `vmm` itself is gated on
    // `cloud-hypervisor` while the VFIO adapter inside it is gated on `vfio`.
    // Both must be on, so this is `all(...)` rather than either alone.
    #[cfg(all(feature = "vfio", feature = "cloud-hypervisor"))]
    fabric.register_adapter(Arc::new(nauti_fabric::vmm::VfioGpuAdapter::new()));
    let reports = fabric.adapter_reports();

    if json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
    } else {
        println!("NAME\tSCOPE\tHEALTHY");
        for report in reports {
            println!("{}\t{}\t{}", report.name, report.scope, report.healthy);
        }
    }

    Ok(())
}

#[cfg(feature = "numa")]
fn topology(json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let topology = nauti_fabric::topology::NumaTopology::discover()?;

    if json {
        println!("{}", serde_json::to_string_pretty(topology.nodes())?);
    } else {
        println!("NODE\tMEMORY_BYTES\tLOGICAL_CPUS\tPCI_DEVICES");
        for node in topology.nodes() {
            println!(
                "{}\t{}\t{}\t{}",
                node.os_index,
                node.local_memory_bytes,
                node.logical_cpus,
                node.pci_devices.len()
            );
            for pci in &node.pci_devices {
                println!(
                    "  - {} (vendor={:?} device={:?})",
                    pci.name, pci.vendor_id, pci.device_id
                );
            }
        }
    }

    Ok(())
}

fn gpus(json: bool, grouped: bool, type_filter: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let discovery = nauti_fabric::gpu::GpuDiscoveryResult::discover()?;

    // Apply the optional vendor filter (--type amd|nvidia|intel|...).
    let filter = type_filter.as_deref().map(str::to_lowercase);

    if json {
        if grouped {
            println!("{}", serde_json::to_string_pretty(&discovery.grouped())?);
        } else {
            println!("{}", serde_json::to_string_pretty(&discovery.devices)?);
        }
        return Ok(());
    }

    if grouped {
        let groups = discovery.grouped();
        for (vendor, devices) in &groups {
            if filter.as_deref().map_or(false, |f| !vendor.to_lowercase().contains(f)) {
                continue;
            }
            if vendor == "__display_only__" {
                continue; // hide BMC VGA controllers unless explicitly asked
            }
            println!("=== {} ({} devices) ===", vendor, devices.len());
            for gpu in devices {
                print_gpu(gpu);
            }
        }
        return Ok(());
    }

    // Flat list, optional vendor filter.
    for gpu in &discovery.devices {
        if gpu.display_only {
            continue;
        }
        if filter.as_deref().map_or(false, |f| !gpu.vendor.label().to_lowercase().contains(f)) {
            continue;
        }
        print_gpu(gpu);
    }
    Ok(())
}

fn print_gpu(gpu: &nauti_fabric::gpu::GpuDevice) {
    let uuid = gpu.uuid.as_deref().unwrap_or("-");
    let util = gpu
        .utilization_pct
        .map(|u| format!("{}%", u))
        .unwrap_or_else(|| "-".into());
    let _temp = gpu
        .temperature_c
        .map(|t| format!("{t}°C"))
        .unwrap_or_else(|| "-".into());
    let vram_mb = gpu.vram_total_bytes / (1024 * 1024);
    let flag = if gpu.display_only { " [display-only]" } else { "" };
    println!(
        "{}\t{}\t{}\t{}MB\t{}\t{}\t{}{}",
        gpu.vendor.label(),
        gpu.device_name,
        uuid,
        vram_mb,
        gpu.driver,
        gpu.pci_bdf,
        util,
        flag,
    );
}

fn inventory(node: &str, json: bool, refresh: bool) -> Result<(), Box<dyn std::error::Error>> {
    let fabric = Fabric::default();
    if refresh {
        // Self-discovering path: re-run HostInventory::discover (+ NVML
        // when the nvidia feature is on), diff against the current
        // fabric, and emit the diff so the operator (or a tool) can
        // audit what changed.
        let report = fabric.refresh_local(node);
        if json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            println!("REPORT: added={} removed={} blocked_by_lease={}",
                report.added.len(), report.removed.len(), report.blocked_by_lease.len());
            if !report.added.is_empty() {
                println!("ADDED:");
                for id in &report.added {
                    println!("  + {id}");
                }
            }
            if !report.removed.is_empty() {
                println!("REMOVED:");
                for id in &report.removed {
                    println!("  - {id}");
                }
            }
            if !report.blocked_by_lease.is_empty() {
                println!("BLOCKED_BY_LEASE (will be removed once lease expires or is released):");
                for id in &report.blocked_by_lease {
                    println!("  ! {id}");
                }
            }
        }
    } else {
        // Original path: discover once, list.
        fabric.discover_local(node);
        let resources = fabric.resources();
        if json {
            println!("{}", serde_json::to_string_pretty(&resources)?);
        } else {
            println!("ID\tKIND\tCAPACITY\tUNIT\tNODE\tSTATE");
            for resource in resources {
                println!(
                    "{}\t{:?}\t{}\t{}\t{}\t{:?}",
                    resource.id,
                    resource.kind,
                    resource.capacity,
                    resource.unit,
                    resource.node,
                    resource.state
                );
            }
        }
    }
    Ok(())
}

fn demo() -> Result<(), Box<dyn std::error::Error>> {
    let fabric = Fabric::default();
    fabric.register(Resource {
        id: "gpu.virtual.0".into(),
        kind: ResourceKind::Gpu,
        capacity: 1,
        unit: "device".into(),
        node: "local".into(),
        state: ResourceState::Available,
        exclusive: true,
        attributes: BTreeMap::from([("adapter".into(), "proof".into())]),
    });
    fabric.register_adapter(Arc::new(LocalProofAdapter));
    let lease = fabric.lease_exclusive("gpu.virtual.0", "forge-demo", Duration::from_secs(30))?;
    let attachment = fabric.attach("local-proof", &lease)?;
    fabric.release(&lease)?;
    println!("lease={} attachment={} released=true", attachment.lease_id, attachment.adapter);
    Ok(())
}

/// Starts a fabric agent process: registers local host inventory plus a
/// demo exclusive GPU resource, then serves the RPC protocol over Iroh/QUIC
/// until Ctrl-C. Prints the JSON-encoded `EndpointAddr` a controller process
/// needs to connect (`nauti agent-connect <addr>`).
fn agent_serve(node: &str) -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
        let fabric = Arc::new(Fabric::default());
        fabric.discover_local(node);
        fabric.register(Resource {
            id: "gpu.virtual.0".into(),
            kind: ResourceKind::Gpu,
            capacity: 1,
            unit: "device".into(),
            node: node.into(),
            state: ResourceState::Available,
            exclusive: true,
            attributes: BTreeMap::from([("adapter".into(), "proof".into())]),
        });
        fabric.register_adapter(Arc::new(LocalProofAdapter));
        fabric.register_adapter(Arc::new(nauti_fabric::FabricStoreAdapter::new()));
        fabric.register_adapter(Arc::new(nauti_fabric::WgpuRemoteAdapter));
        fabric.register_adapter(Arc::new(nauti_fabric::ResearchMemoryAdapter));
        fabric.register_adapter(Arc::new(nauti_fabric::hwinfo::RdmaAdapter::default()));
        #[cfg(all(feature = "vfio", feature = "cloud-hypervisor"))]
        fabric.register_adapter(Arc::new(nauti_fabric::vmm::VfioGpuAdapter::new()));

        let secret_key = match load_or_create_agent_key(node) {
            Ok(key) => Some(key),
            Err(error) => {
                println!("warning: agent identity not persisted ({error}); using an ephemeral key");
                None
            }
        };
        let (router, addr) = nauti_fabric::rpc::serve_with_identity(
            fabric,
            Arc::new(nauti_fabric::rpc_auth::NoAuth),
            secret_key,
        )
        .await?;
        println!("nauti agent listening; endpoint address (paste into agent-connect):");
        println!("{}", serde_json::to_string(&addr)?);
        match persist_addr(&node, &addr) {
            Ok(path) => println!("self-registered address file: {path}"),
            Err(error) => println!("warning: could not persist address file: {error}"),
        }
        println!("press ctrl-c to stop");

        tokio::signal::ctrl_c().await?;
        router.shutdown().await.map_err(|error| format!("router shutdown failed: {error}"))?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}

/// Connects to a remote fabric agent and runs the two-process lease/attach/
/// release proof against it end-to-end over authenticated Iroh/QUIC.
fn agent_connect(addr_json: &str) -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let addr: iroh::EndpointAddr = serde_json::from_str(addr_json)?;
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
        let mut client = AgentClient::connect(addr).await.map_err(|error| error.to_string())?;

        let pong = client.call(RpcRequest::Ping).await.map_err(|error| error.to_string())?;
        println!("ping -> {pong:?}");

        let inventory = client.call(RpcRequest::Inventory).await.map_err(|error| error.to_string())?;
        println!("inventory -> {inventory:?}");

        let lease_response = client
            .call(RpcRequest::LeaseExclusive {
                resource_id: "gpu.virtual.0".into(),
                owner: "agent-connect-proof".into(),
                ttl_secs: 30,
            })
            .await
            .map_err(|error| error.to_string())?;
        let lease = match lease_response {
            RpcResponse::Leased(lease) => lease,
            other => return Err(format!("expected Leased response, got {other:?}").into()),
        };
        println!("lease-exclusive -> {lease:?}");

        let attach_response = client
            .call(RpcRequest::Attach { adapter: "local-proof".into(), lease: lease.clone() })
            .await
            .map_err(|error| error.to_string())?;
        println!("attach -> {attach_response:?}");

        let release_response =
            client.call(RpcRequest::Release(lease)).await.map_err(|error| error.to_string())?;
        println!("release -> {release_response:?}");

        client.close().await.map_err(|error| error.to_string())?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}
// ---------------------------------------------------------------------------
// vhost-user CLI (feature-gated, M7)
// ---------------------------------------------------------------------------

/// Serve one virtio-rng backend; blocks until Cloud Hypervisor disconnects.
#[cfg(feature = "vhost-user")]
fn vhost_serve(
    socket: std::path::PathBuf,
    source: std::path::PathBuf,
    period_ms: u128,
    max_bytes: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = nauti_fabric::vhost::RngConfig {
        socket_path: socket.clone(),
        source_path: source,
        period_ms,
        max_bytes,
    };
    println!("serving virtio-rng backend on {}", socket.display());
    println!(
        "attach with: nauti vm launch --user-device {} ...",
        socket.display()
    );
    nauti_fabric::vhost::serve_rng(config)?;
    Ok(())
}

/// Remote entropy server (device-host half of the stub).
#[cfg(feature = "vhost-user")]
fn vhost_entropy_serve(listen: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("entropy server listening on {listen} (length-prefixed requests)");
    nauti_fabric::vhost::entropy_serve(listen)?;
    Ok(())
}

/// Remote entropy pump (VM-host half of the stub).
#[cfg(feature = "vhost-user")]
fn vhost_pump(
    connect: &str,
    out: &std::path::Path,
    chunk: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("pumping entropy from {connect} into {}", out.display());
    nauti_fabric::vhost::entropy_pump(connect, out, chunk)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Cloud Hypervisor CLI (feature-gated)
// ---------------------------------------------------------------------------

#[cfg(feature = "cloud-hypervisor")]
struct VmLaunchArgs {
    resource_id: String,
    binary: PathBuf,
    api_socket: PathBuf,
    kernel: PathBuf,
    rootfs: PathBuf,
    vm_name: Option<String>,
    vcpus: String,
    memory_mib: String,
    virtio_blk: Option<PathBuf>,
    net: Option<String>,
    user_device: Option<PathBuf>,
    fs: Option<String>,
    vsock: Option<String>,
    watchdog: bool,
    cmdline: Option<String>,
    initramfs: Option<PathBuf>,
    rng_src: Option<String>,
    shared: bool,
    ttl_secs: u64,
}

#[cfg(feature = "cloud-hypervisor")]
fn vm_capability() -> Result<(), Box<dyn std::error::Error>> {
    // Probe the default binary path; the operator can override via the
    // `CH_BINARY` env var or by setting `--binary` on the `launch` subcommand.
        let default = std::env::var("CH_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|_| which::which("cloud-hypervisor").unwrap_or_else(|_| {
            PathBuf::from("/usr/bin/cloud-hypervisor")
        }));
    let adapter = CloudHypervisorAdapter::new(default);
    let report = adapter.capability_report();
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !report.healthy {
        eprintln!(
            "warning: cloud-hypervisor adapter is unhealthy; \
             `nauti vm launch` will fail until the binary is reachable"
        );
    }
    Ok(())
}

#[cfg(feature = "cloud-hypervisor")]
fn vm_launch(args: VmLaunchArgs) -> Result<(), Box<dyn std::error::Error>> {
    let VmLaunchArgs {
        resource_id,
        binary,
        api_socket,
        kernel,
        rootfs,
        vm_name,
        vcpus,
        memory_mib,
        virtio_blk,
        net,
        user_device,
        fs,
        vsock,
        watchdog,
        cmdline,
        initramfs,
        rng_src,
        shared,
        ttl_secs,
    } = args;
    let spec = VmResourceSpec {
        api_socket: api_socket.display().to_string(),
        kernel: kernel.display().to_string(),
        rootfs: rootfs.display().to_string(),
        vm_name,
        vcpus: Some(vcpus),
        memory_mib: Some(memory_mib),
        virtio_blk: virtio_blk.as_ref().map(|path| path.display().to_string()),
        net,
        user_device: user_device.map(|path| path.display().to_string()),
        user_device_type: None,
        user_device_queue_sizes: None,
        fs,
        vsock,
        watchdog: watchdog.then(|| "on".into()),
        cmdline,
        initramfs: initramfs.map(|path| path.display().to_string()),
        rng_src,
        shared: shared.then(|| "on".into()),
    };
    let mut attributes = spec.into_attributes();
    attributes.insert("vmm.binary".into(), binary.display().to_string());

    let fabric = Fabric::default();
    fabric.register(Resource {
        id: resource_id.clone(),
        kind: ResourceKind::Device,
        capacity: 1,
        unit: "vm".into(),
        node: "local".into(),
        state: ResourceState::Available,
        exclusive: true,
        attributes,
    });
    let adapter = Arc::new(CloudHypervisorAdapter::new(binary));
    fabric.register_adapter(Arc::clone(&adapter) as Arc<dyn nauti_fabric::ResourceAdapter>);

    let lease = fabric.lease_exclusive(&resource_id, "nauti-vm", Duration::from_secs(ttl_secs))?;
    let attachment = fabric.attach("cloud-hypervisor", &lease)?;
    println!("{}", serde_json::to_string_pretty(&attachment)?);
    Ok(())
}

#[cfg(feature = "cloud-hypervisor")]
fn vm_reconcile() -> Result<(), Box<dyn std::error::Error>> {
    // Without a live process we cannot recover the adapter's managed-VM
    // set across invocations; reconcile here is a no-op that prints a hint.
    // The `nauti` binary's typical use is to launch and reconcile within a
    // single process, so the adapter is the in-process authority.
    eprintln!(
        "nauti vm reconcile is a no-op across processes: run reconcile from \
         the same process that launched the VM, or use `ch-remote \
         --api-socket <sock> remove-vm <name>` directly."
    );
    Ok(())
}
// ---------------------------------------------------------------------------
// Fabric RPC verbs (M5: the LLM-facing tool layer calls these)
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
enum FabricAction {
    /// Liveness probe: Ping round-trip. Non-zero exit when unreachable.
    Ping { addr: String },
    /// List every resource registered with the remote fabric, as JSON.
    Inventory { addr: String },
    /// Query the remote fabric for available resources matching the request.
    Find {
        addr: String,
        /// Resource kind filter (cpu, gpu, memory, storage, network, device).
        #[arg(long)]
        kind: Option<String>,
        /// Minimum capacity in the resource's own unit.
        #[arg(long)]
        min_capacity: Option<u64>,
        /// Restrict to one node name.
        #[arg(long)]
        node: Option<String>,
        /// Required attribute, `key=value`; repeatable.
        #[arg(long = "attr")]
        attrs: Vec<String>,
        /// Only exclusive-capable resources.
        #[arg(long)]
        exclusive: bool,
    },
    /// Take an exclusive, time-bounded lease on one resource; prints the
    /// lease as JSON (feed it back to `fabric attach` / `fabric release`).
    Lease {
        addr: String,
        #[arg(long)]
        resource_id: String,
        #[arg(long, default_value = "mcp-tools")]
        owner: String,
        #[arg(long, default_value_t = 300)]
        ttl_secs: u64,
    },
    /// Attach a leased resource through a named adapter (M8 store detach
    /// needs a live attachment so `release` can call detach).
    Attach {
        addr: String,
        #[arg(long, default_value = "fabric-store")]
        adapter: String,
        /// The lease JSON printed by `fabric lease`.
        #[arg(long = "lease-json")]
        lease_json: String,
    },
    /// Release a lease (pass the lease JSON printed by `fabric lease`).
    /// Detaches any recorded attachment first.
    Release {
        addr: String,
        /// The lease JSON printed by `fabric lease`.
        #[arg(long = "lease-json")]
        lease_json: String,
    },
}

/// Runs one fabric verb against a remote agent over authenticated Iroh/QUIC.
/// Every failure path returns a typed, printable error — the tool layer
/// surfaces these verbatim, so unreachable agents degrade instead of panicking.
fn fabric_action(action: FabricAction) -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async {
        let addr: String = match &action {
            FabricAction::Ping { addr }
            | FabricAction::Inventory { addr }
            | FabricAction::Find { addr, .. }
            | FabricAction::Lease { addr, .. }
            | FabricAction::Attach { addr, .. }
            | FabricAction::Release { addr, .. } => addr.clone(),
        };
        let addr: iroh::EndpointAddr = serde_json::from_str(&addr)
            .map_err(|error| format!("--addr is not a JSON EndpointAddr: {error}"))?;
        let mut client = AgentClient::connect(addr)
            .await
            .map_err(|error| format!("agent unreachable: {error}"))?;
        let result: Result<String, String> = async {
            match action {
                FabricAction::Ping { .. } => client
                    .call(RpcRequest::Ping)
                    .await
                    .map(|pong| format!("pong -> {pong:?}"))
                    .map_err(|error| error.to_string()),
                FabricAction::Inventory { .. } => {
                    let response = client.call(RpcRequest::Inventory).await.map_err(|e| e.to_string())?;
                    match response {
                        RpcResponse::Inventory(resources) => serde_json::to_string_pretty(&resources)
                            .map_err(|e| e.to_string()),
                        other => Err(format!("expected Inventory response, got {other:?}")),
                    }
                }
                FabricAction::Find { kind, min_capacity, node, attrs, exclusive, .. } => {
                    let mut required_attributes = BTreeMap::new();
                    for attr in attrs {
                        let (key, value) = attr
                            .split_once('=')
                            .ok_or_else(|| format!("--attr expects key=value, got {attr:?}"))?;
                        required_attributes.insert(key.to_string(), value.to_string());
                    }
                    let kind = match kind.as_deref() {
                        None => None,
                        Some(raw) => Some(match raw.to_ascii_lowercase().as_str() {
                            "cpu" => ResourceKind::Cpu,
                            "gpu" => ResourceKind::Gpu,
                            "memory" => ResourceKind::Memory,
                            "storage" => ResourceKind::Storage,
                            "network" => ResourceKind::Network,
                            "device" => ResourceKind::Device,
                            other => return Err(format!("unknown kind {other:?} (cpu|gpu|memory|storage|network|device)")),
                        }),
                    };
                    let request = ResourceRequest {
                        kind,
                        minimum_capacity: min_capacity,
                        node,
                        required_attributes,
                        exclusive,
                    };
                    let response = client
                        .call(RpcRequest::FindAvailable(request))
                        .await
                        .map_err(|e| e.to_string())?;
                    match response {
                        RpcResponse::FindAvailable(resources) => serde_json::to_string_pretty(&resources)
                            .map_err(|e| e.to_string()),
                        other => Err(format!("expected FindAvailable response, got {other:?}")),
                    }
                }
                FabricAction::Lease { resource_id, owner, ttl_secs, .. } => {
                    let response = client
                        .call(RpcRequest::LeaseExclusive { resource_id, owner, ttl_secs })
                        .await
                        .map_err(|e| e.to_string())?;
                    match response {
                        RpcResponse::Leased(lease) => serde_json::to_string_pretty(&lease)
                            .map_err(|e| e.to_string()),
                        RpcResponse::Error(error) => Err(format!("lease rejected: {}", error.message)),
                        other => Err(format!("expected Leased response, got {other:?}")),
                    }
                }
                FabricAction::Attach { adapter, lease_json, .. } => {
                    let lease: Lease = serde_json::from_str(&lease_json)
                        .map_err(|error| format!("lease JSON is not a Lease: {error}"))?;
                    let response = client
                        .call(RpcRequest::Attach { adapter, lease })
                        .await
                        .map_err(|e| e.to_string())?;
                    match response {
                        RpcResponse::Attached(attachment) => serde_json::to_string_pretty(&attachment)
                            .map_err(|e| e.to_string()),
                        RpcResponse::Error(error) => Err(format!("attach rejected: {}", error.message)),
                        other => Err(format!("expected Attached response, got {other:?}")),
                    }
                }
                FabricAction::Release { lease_json, .. } => {
                    let lease: Lease = serde_json::from_str(&lease_json)
                        .map_err(|error| format!("lease JSON is not a Lease: {error}"))?;
                    let response = client.call(RpcRequest::Release(lease)).await.map_err(|e| e.to_string())?;
                    match response {
                        RpcResponse::Released => Ok("released=true".into()),
                        RpcResponse::Error(error) => Err(format!("release rejected: {}", error.message)),
                        other => Err(format!("expected Released response, got {other:?}")),
                    }
                }
            }
        }
        .await;
        client.close().await.map_err(|error| error.to_string())?;
        println!("{}", result?);
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}

/// Default address-file directory for self-registered agents; overridable
/// with `NAUTI_AGENT_ADDR_DIR` (tests, sandboxed deployments).
fn addr_file_dir() -> String {
    std::env::var("NAUTI_AGENT_ADDR_DIR").unwrap_or_else(|_| "/var/lib/nauti".into())
}

/// Agent identity file: `<dir>/<node>-secret.key` (32 raw bytes, 0600).
fn agent_key_path(node: &str) -> String {
    format!("{}/{}-secret.key", addr_file_dir().trim_end_matches('/'), node)
}

fn load_or_create_agent_key(node: &str) -> Result<iroh::SecretKey, String> {
    load_or_create_key_at(&agent_key_path(node))
}

fn load_or_create_key_at(path: &str) -> Result<iroh::SecretKey, String> {
    use std::os::unix::fs::OpenOptionsExt;
    if let Ok(bytes) = std::fs::read(path) {
        let raw: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| format!("{path}: expected 32 bytes, found {}", bytes.len()))?;
        return Ok(iroh::SecretKey::from_bytes(&raw));
    }
    let key = iroh::SecretKey::generate();
    if let Some(parent) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("{path}: {e}"))?;
    std::io::Write::write_all(&mut file, &key.to_bytes()).map_err(|e| format!("{path}: {e}"))?;
    Ok(key)
}

/// The address-file path for a node: `<dir>/<node>-addr.json`.
fn addr_file_path(node: &str) -> String {
    format!("{}/{}-addr.json", addr_file_dir().trim_end_matches('/'), node)
}

/// Persists the agent's `EndpointAddr` so controller processes (the MCP tool
/// layer) can self-discover the fleet without manual endpoint sync. The node
/// already knows what it is — it announces where to reach it.
fn persist_addr(node: &str, addr: &iroh::EndpointAddr) -> Result<String, String> {
    let json = serde_json::to_string_pretty(addr).map_err(|error| error.to_string())?;
    persist_addr_json_to(&addr_file_path(node), &json)
}

fn persist_addr_json_to(path: &str, json: &str) -> Result<String, String> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    std::fs::write(path, json).map_err(|error| format!("{path}: {error}"))?;
    Ok(path.to_string())
}

/// `(node, raw addr JSON)` for every `<node>-addr.json` in the addr dir.
fn fleet_addr_files() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir(addr_file_dir()) else { return out };
    for entry in dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(node) = name.strip_suffix("-addr.json") {
            if let Ok(raw) = std::fs::read_to_string(entry.path()) {
                out.push((node.to_string(), raw));
            }
        }
    }
    out.sort();
    out
}

/// Label a direct IP path by its prefix.
fn path_label(ip: &str) -> Option<&'static str> {
    if ip.starts_with("10.0.10.") {
        Some("25g")
    } else if ip.starts_with("10.44.0.") {
        Some("lan-10.44")
    } else if ip.starts_with("192.168.") {
        Some("lan")
    } else if ip.starts_with("100.") {
        Some("tailscale")
    } else {
        None
    }
}

/// `(min, avg, max)` ms from `ping -q` output.
fn parse_ping_rtt(output: &str) -> Option<(f64, f64, f64)> {
    let line = output.lines().find(|l| l.contains("min/avg/max"))?;
    let nums = line.split('=').nth(1)?.trim().split_whitespace().next()?;
    let v: Vec<f64> = nums.split('/').filter_map(|n| n.parse().ok()).collect();
    (v.len() >= 3).then(|| (v[0], v[1], v[2]))
}

fn icmp_paths(addr: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for item in addr["addrs"].as_array().into_iter().flatten() {
        let Some(sock) = item["Ip"].as_str() else { continue };
        let ip = sock.rsplit_once(':').map(|(h, _)| h).unwrap_or(sock).trim_matches(['[', ']']);
        let Some(label) = path_label(ip) else { continue };
        if !seen.insert(ip.to_string()) {
            continue;
        }
        let output = std::process::Command::new("ping")
            .args(["-c", "5", "-i", "0.2", "-W", "1", "-q", ip])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        let rtt = parse_ping_rtt(&output);
        out.push(serde_json::json!({
            "path": label,
            "ip": ip,
            "reachable": rtt.is_some(),
            "rtt_ms": rtt.map(|(min, avg, max)| serde_json::json!({"min": min, "avg": avg, "max": max})),
        }));
    }
    out
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Roll one node's inventory into the fleet summary shape.
fn summarize_inventory(resources: &[Resource]) -> serde_json::Value {
    let attr = |r: &Resource, k: &str| r.attributes.get(k).cloned().unwrap_or_default();
    let cpus = resources.iter().find(|r| r.id == "local.cpu.logical").map(|r| r.capacity).unwrap_or(0);
    let memory = resources.iter().find(|r| r.id == "local.memory").map(|r| r.capacity).unwrap_or(0);
    let gpus: Vec<_> = resources
        .iter()
        .filter(|r| r.kind == ResourceKind::Gpu && r.id.starts_with("local.gpu.") && !r.attributes.contains_key("gpu.transport"))
        .map(|r| serde_json::json!({
            "id": r.id,
            "name": attr(r, "gpu.device_name"),
            "vendor": attr(r, "gpu.vendor"),
            "driver": attr(r, "gpu.driver"),
            "pci_bdf": attr(r, "gpu.pci_bdf"),
            "numa_node": attr(r, "gpu.numa_node"),
            "vram_bytes": r.capacity,
        }))
        .collect();
    let rdma: Vec<_> = resources
        .iter()
        .filter(|r| r.id.starts_with("local.rdma."))
        .map(|r| serde_json::json!({
            "device": attr(r, "rdma.device"),
            "state": attr(r, "rdma.state"),
            "rate": attr(r, "rdma.rate"),
            "transport": attr(r, "rdma.transport"),
            "netdev": attr(r, "rdma.netdev"),
            "numa_node": attr(r, "rdma.numa_node"),
        }))
        .collect();
    let links: Vec<_> = resources
        .iter()
        .filter(|r| r.id.starts_with("local.network.") && !r.attributes.contains_key("net.virtual"))
        .filter(|r| r.attributes.get("net.operstate").map(|s| s == "up").unwrap_or(false))
        .map(|r| serde_json::json!({
            "interface": attr(r, "interface"),
            "speed_mbps": attr(r, "net.speed_mbps"),
            "ipv4": attr(r, "net.ipv4"),
            "rdma_device": attr(r, "net.rdma_device"),
            "numa_node": attr(r, "net.numa_node"),
        }))
        .collect();
    let numa_nodes = resources.iter().filter(|r| r.id.starts_with("local.numa.")).count();
    serde_json::json!({
        "logical_cpus": cpus,
        "memory_bytes": memory,
        "numa_nodes": numa_nodes,
        "gpus": gpus,
        "rdma_ports": rdma,
        "links_up": links,
    })
}

async fn probe_node(node: String, raw: String, samples: u32, paths: bool) -> serde_json::Value {
    let addr_value: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
    let path_timings = if paths {
        let v = addr_value.clone();
        tokio::task::spawn_blocking(move || icmp_paths(&v)).await.unwrap_or_default()
    } else {
        Vec::new()
    };
    let addr: iroh::EndpointAddr = match serde_json::from_str(&raw) {
        Ok(a) => a,
        Err(e) => return serde_json::json!({"node": node, "online": false, "error": format!("bad addr file: {e}"), "paths": path_timings}),
    };
    let started = std::time::Instant::now();
    let connect = tokio::time::timeout(Duration::from_secs(15), AgentClient::connect(addr)).await;
    let mut client = match connect {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => return serde_json::json!({"node": node, "online": false, "error": e.to_string(), "paths": path_timings}),
        Err(_) => return serde_json::json!({"node": node, "online": false, "error": "connect timeout 15s", "paths": path_timings}),
    };
    let connect_ms = started.elapsed().as_secs_f64() * 1000.0;
    let mut rtts = Vec::new();
    for _ in 0..samples.max(1) {
        let t = std::time::Instant::now();
        match tokio::time::timeout(Duration::from_secs(5), client.call(RpcRequest::Ping)).await {
            Ok(Ok(RpcResponse::Pong)) => rtts.push(t.elapsed().as_secs_f64() * 1000.0),
            _ => break,
        }
    }
    rtts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let inventory = match tokio::time::timeout(Duration::from_secs(10), client.call(RpcRequest::Inventory)).await {
        Ok(Ok(RpcResponse::Inventory(resources))) => summarize_inventory(&resources),
        _ => serde_json::Value::Null,
    };
    let _ = client.close().await;
    serde_json::json!({
        "node": node,
        "online": !rtts.is_empty(),
        "connect_ms": connect_ms,
        "rpc_rtt_ms": {
            "samples": rtts.len(),
            "min": rtts.first().copied().unwrap_or(0.0),
            "median": percentile(&rtts, 0.5),
            "p95": percentile(&rtts, 0.95),
            "max": rtts.last().copied().unwrap_or(0.0),
        },
        "paths": path_timings,
        "resources": inventory,
    })
}

fn fleet(json: bool, samples: u32, paths: bool, out: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let from = hostname_short();
    let nodes = runtime.block_on(async {
        let tasks: Vec<_> = fleet_addr_files()
            .into_iter()
            .map(|(node, raw)| tokio::spawn(probe_node(node, raw, samples, paths)))
            .collect();
        let mut nodes = Vec::new();
        for task in tasks {
            if let Ok(v) = task.await {
                nodes.push(v);
            }
        }
        nodes
    });
    let online: Vec<&serde_json::Value> = nodes.iter().filter(|n| n["online"] == true).collect();
    let sum = |f: &dyn Fn(&serde_json::Value) -> u64| online.iter().map(|n| f(&n["resources"])).sum::<u64>();
    let totals = serde_json::json!({
        "nodes": nodes.len(),
        "online": online.len(),
        "logical_cpus": sum(&|r| r["logical_cpus"].as_u64().unwrap_or(0)),
        "memory_bytes": sum(&|r| r["memory_bytes"].as_u64().unwrap_or(0)),
        "gpus": sum(&|r| r["gpus"].as_array().map(|a| a.len() as u64).unwrap_or(0)),
        "vram_bytes": sum(&|r| r["gpus"].as_array().map(|a| a.iter().map(|g| g["vram_bytes"].as_u64().unwrap_or(0)).sum()).unwrap_or(0)),
        "rdma_ports_active": sum(&|r| r["rdma_ports"].as_array().map(|a| a.iter().filter(|p| p["state"] == "ACTIVE").count() as u64).unwrap_or(0)),
    });
    let view = serde_json::json!({
        "kind": "nauti_fleet_view",
        "from": from,
        "generated_at": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        "totals": totals,
        "nodes": nodes,
    });
    let path = out.unwrap_or_else(|| format!("{}/fleet-view-{}.json", addr_file_dir().trim_end_matches('/'), from));
    if let Err(e) = persist_addr_json_to(&path, &serde_json::to_string_pretty(&view)?) {
        eprintln!("warning: could not write fleet view: {e}");
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&view)?);
        return Ok(());
    }
    let gib = |b: u64| b as f64 / (1u64 << 30) as f64;
    println!(
        "fleet from {from}: {}/{} online | {} cpus | {:.0} GiB ram | {} gpus {:.0} GiB vram | {} rdma ports active",
        totals["online"], totals["nodes"], totals["logical_cpus"], gib(totals["memory_bytes"].as_u64().unwrap_or(0)),
        totals["gpus"], gib(totals["vram_bytes"].as_u64().unwrap_or(0)), totals["rdma_ports_active"],
    );
    println!("{:<14} {:>6} {:>9} {:>9} {:>9} {:>5} {:>5}  paths", "node", "online", "rpc_min", "rpc_med", "rpc_p95", "gpus", "rdma");
    for n in &nodes {
        let r = &n["rpc_rtt_ms"];
        let res = &n["resources"];
        let paths: Vec<String> = n["paths"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|p| match p["rtt_ms"]["avg"].as_f64() {
                Some(avg) => format!("{}={:.2}ms", p["path"].as_str().unwrap_or("?"), avg),
                None => format!("{}=down", p["path"].as_str().unwrap_or("?")),
            })
            .collect();
        println!(
            "{:<14} {:>6} {:>8.2}ms {:>8.2}ms {:>8.2}ms {:>5} {:>5}  {}",
            n["node"].as_str().unwrap_or("?"),
            if n["online"] == true { "yes" } else { "NO" },
            r["min"].as_f64().unwrap_or(0.0),
            r["median"].as_f64().unwrap_or(0.0),
            r["p95"].as_f64().unwrap_or(0.0),
            res["gpus"].as_array().map(|a| a.len()).unwrap_or(0),
            res["rdma_ports"].as_array().map(|a| a.iter().filter(|p| p["state"] == "ACTIVE").count()).unwrap_or(0),
            if n["online"] == true { paths.join(" ") } else { n["error"].as_str().unwrap_or("").to_string() },
        );
    }
    Ok(())
}

fn hostname_short() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|h| h.trim().split('.').next().unwrap_or("local").to_string())
        .unwrap_or_else(|_| "local".into())
}

fn wgpu_serve(bind: &str) -> Result<(), Box<dyn std::error::Error>> {
    let addr = nauti_fabric::gpu::wgpu_remote::parse_bind(bind)?;
    nauti_fabric::gpu::wgpu_remote::serve(addr).map_err(|e| e.into())
}

fn wgpu_run(connect: &str, n: usize) -> Result<(), Box<dyn std::error::Error>> {
    let addr = nauti_fabric::gpu::wgpu_remote::parse_bind(connect)?;
    let job = nauti_fabric::gpu::wgpu_remote::sample_job(n);
    let result = nauti_fabric::gpu::wgpu_remote::submit(addr, &job)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    if result.status != "ok" {
        return Err(format!("wgpu-remote job status {}", result.status).into());
    }
    Ok(())
}

fn research_discover(json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let report = nauti_fabric::ResearchMemoryReport::discover();
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "cxl_bus={} dax={} famfs={} drust={} claimed={}",
            report.cxl_bus,
            report.dax_nodes.len(),
            report.famfs_mounts.len(),
            report.drust_present,
            report.claimed_as_live_ram
        );
    }
    Ok(())
}

/// M11 — one ask, one fragment. Both subcommands print the fragment or the
/// typed reject and exit non-zero on a reject, so a caller (or a model) cannot
/// read a refusal as success.
fn plan_action(action: PlanAction) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        PlanAction::Ask { role, head, node, timeout_secs, json } => {
            plan_ask(&role, head, node, timeout_secs, json)
        }
        PlanAction::Offline { role, view, node, roles, matrix, planner, json } => {
            plan_offline(&role, &view, node, &roles, matrix, &planner, json)
        }
    }
}

/// Print a fragment or a typed reject. The reject always shows its rRCP-shaped
/// reason token and, when the head supplied one, its HTTP status.
fn report_plan(
    result: Result<nauti_fabric::plan::PlanFragment, nauti_fabric::plan::PlanReject>,
    json: bool,
    planner: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use nauti_fabric::plan::PlanReject;
    match result {
        Ok(fragment) => {
            if json {
                let mut value = serde_json::to_value(&fragment)?;
                value["planner"] = serde_json::Value::String(planner.to_string());
                println!("{}", serde_json::to_string_pretty(&value)?);
            } else {
                println!(
                    "plan fragment role={} host={} slot={} model={}{}",
                    fragment.role,
                    fragment.host_key,
                    fragment.slot,
                    fragment.model_id,
                    match fragment.rtt_ms {
                        Some(ms) => format!(" rtt={ms:.3}ms"),
                        None => " rtt=not-measured".to_string(),
                    }
                );
                println!("  lease to request: {}", fragment.resource_id.as_deref().unwrap_or("(snapshot named none)"));
                println!("  reason: {}", fragment.reason);
                println!("  planner: {planner}");
            }
            Ok(())
        }
        Err(err) => {
            let status = err
                .http_status()
                .map(|s| format!("http={s} "))
                .unwrap_or_default();
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "status": "rejected",
                        "reason": err.reason_token(),
                        "http_status": err.http_status(),
                        "detail": err.to_string(),
                        "planner": planner,
                    }))?
                );
            } else {
                eprintln!("plan rejected: {status}{}", err);
                eprintln!("  reason token: {}", err.reason_token());
                if let PlanReject::EscalateNowhere { .. } = err {
                    eprintln!("  no host in the fleet can take this role; not an empty plan");
                }
            }
            Err(format!("plan rejected: {}", err.reason_token()).into())
        }
    }
}

fn plan_ask(
    role: &str,
    head: Option<String>,
    node: Option<String>,
    timeout_secs: u64,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use nauti_fabric::plan::{ask_head_with_timeout, PlanRequest};
    // No clap `env` feature in this crate, so the fallback is read here.
    let head = head
        .or_else(|| std::env::var("NAUTI_PLAN_HEAD").ok())
        .ok_or("need --head URL or NAUTI_PLAN_HEAD")?;
    let node_id = node.unwrap_or_else(hostname_short);
    let req = PlanRequest::new(node_id, role);
    let result = ask_head_with_timeout(&head, &req, Duration::from_secs(timeout_secs.max(1)));
    // The head owns the decision; name it as the planner of record.
    report_plan(result, json, "head:/v1/plan")
}

fn plan_offline(
    role: &str,
    view_path: &str,
    node: Option<String>,
    roles_spec: &str,
    matrix_path: Option<String>,
    planner_kind: &str,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use nauti_fabric::plan::{FleetView, LatencyMatrix, PlanRequest, RoleCatalogue};
    let node_id = node.unwrap_or_else(hostname_short);
    let mut view = FleetView::load_snapshot(std::path::Path::new(view_path))?;
    if let Some(path) = matrix_path {
        view = view.with_latency(LatencyMatrix::load(std::path::Path::new(&path))?);
    }
    // Roles: a JSON file (array or FALLBACK_CHAIN object) or a comma list.
    let roles = if let Ok(raw) = std::fs::read_to_string(roles_spec) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) {
            RoleCatalogue::from_json(&value)
        } else {
            RoleCatalogue::new(roles_spec.split(',').map(|s| s.trim().to_string()))
        }
    } else {
        RoleCatalogue::new(roles_spec.split(',').map(|s| s.trim().to_string()))
    };
    let req = PlanRequest::new(node_id, role);
    // `monolithic` runs the head's order (local plan first, then fleet-wide);
    // `federated` consults only this node and so has no remote step. Either
    // way a role that cannot be satisfied is a typed 403, never an empty
    // fragment.
    let result = nauti_fabric::plan::plan_by_kind(planner_kind, &req, &view, &roles);
    report_plan(result, json, planner_kind)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_rtt_parses_iputils_summary() {
        let out = "5 packets transmitted, 5 received, 0% packet loss, time 804ms\n\
                   rtt min/avg/max/mdev = 0.112/0.145/0.201/0.031 ms\n";
        assert_eq!(parse_ping_rtt(out), Some((0.112, 0.145, 0.201)));
        assert_eq!(parse_ping_rtt("100% packet loss"), None);
    }

    #[test]
    fn path_labels_cover_fleet_prefixes() {
        assert_eq!(path_label("10.0.10.204"), Some("25g"));
        assert_eq!(path_label("10.44.0.12"), Some("lan-10.44"));
        assert_eq!(path_label("192.168.4.29"), Some("lan"));
        assert_eq!(path_label("100.72.182.77"), Some("tailscale"));
        assert_eq!(path_label("172.17.0.1"), None);
        assert_eq!(path_label("64.131.14.25"), None);
    }

    #[test]
    fn inventory_summary_counts_gpus_rdma_and_links() {
        let mk = |id: &str, kind: ResourceKind, cap: u64, attrs: &[(&str, &str)]| Resource {
            id: id.into(),
            kind,
            capacity: cap,
            unit: "x".into(),
            node: "n".into(),
            state: ResourceState::Available,
            exclusive: false,
            attributes: attrs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        };
        let res = vec![
            mk("local.cpu.logical", ResourceKind::Cpu, 48, &[]),
            mk("local.memory", ResourceKind::Memory, 64, &[]),
            mk("local.numa.0", ResourceKind::Memory, 32, &[]),
            mk("local.numa.1", ResourceKind::Memory, 32, &[]),
            mk("local.gpu.0000_5e_00_0", ResourceKind::Gpu, 16, &[("gpu.device_name", "P100")]),
            mk("gpu.virtual.0", ResourceKind::Gpu, 1, &[]),
            mk("local.gpu.wgpu-remote", ResourceKind::Gpu, 1, &[("gpu.transport", "tailscale")]),
            mk("local.rdma.rocep1s0f0.1", ResourceKind::Network, 25, &[("rdma.state", "ACTIVE")]),
            mk("local.network.enp1s0f0np0", ResourceKind::Network, 25, &[("interface", "enp1s0f0np0"), ("net.operstate", "up")]),
            mk("local.network.docker0", ResourceKind::Network, 0, &[("net.operstate", "up"), ("net.virtual", "true")]),
        ];
        let s = summarize_inventory(&res);
        assert_eq!(s["logical_cpus"], 48);
        assert_eq!(s["numa_nodes"], 2);
        assert_eq!(s["gpus"].as_array().unwrap().len(), 1, "virtual proof and wgpu-remote GPUs are not hardware");
        assert_eq!(s["rdma_ports"].as_array().unwrap().len(), 1);
        assert_eq!(s["links_up"].as_array().unwrap().len(), 1, "virtual links excluded");
    }

    #[test]
    fn percentile_edges() {
        assert_eq!(percentile(&[], 0.5), 0.0);
        assert_eq!(percentile(&[1.0, 2.0, 3.0], 0.5), 2.0);
        assert_eq!(percentile(&[1.0, 2.0, 3.0], 0.95), 3.0);
    }

    #[test]
    fn agent_key_is_created_once_and_reloaded() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("nauti-key-test-{}", std::process::id()));
        let path = dir.join("n-secret.key");
        let _ = std::fs::remove_dir_all(&dir);
        let first = load_or_create_key_at(path.to_str().unwrap()).unwrap();
        let second = load_or_create_key_at(path.to_str().unwrap()).unwrap();
        assert_eq!(first.public(), second.public());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        std::fs::write(&path, b"short").unwrap();
        assert!(load_or_create_key_at(path.to_str().unwrap()).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_addr_json_writes_file_and_creates_parents() {
        let dir = std::env::temp_dir().join(format!("nauti-addr-test-{}", std::process::id()));
        let path = dir.join("nested").join("test-node-addr.json");
        let written = persist_addr_json_to(path.to_str().unwrap(), r#"{"probe":true}"#)
            .expect("persist");
        let read = std::fs::read_to_string(&written).expect("read back");
        assert_eq!(read, r#"{"probe":true}"#);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn addr_file_path_uses_node_name_in_default_dir() {
        // No env mutation (tests run in parallel); verify the default layout.
        if std::env::var("NAUTI_AGENT_ADDR_DIR").is_err() {
            assert_eq!(addr_file_path("t440"), "/var/lib/nauti/t440-addr.json");
            assert_eq!(addr_file_path("cesarops2"), "/var/lib/nauti/cesarops2-addr.json");
        }
    }
}
