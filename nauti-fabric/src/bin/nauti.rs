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

        let (router, addr) = nauti_fabric::rpc::serve(fabric).await?;
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
    /// lease as JSON (feed it back to `fabric release`).
    Lease {
        addr: String,
        #[arg(long)]
        resource_id: String,
        #[arg(long, default_value = "mcp-tools")]
        owner: String,
        #[arg(long, default_value_t = 300)]
        ttl_secs: u64,
    },
    /// Release a lease (pass the lease JSON printed by `fabric lease`).
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

#[cfg(test)]
mod tests {
    use super::*;

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
