use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use nauti_fabric::rpc::{AgentClient, RpcRequest, RpcResponse};
#[cfg(feature = "cloud-hypervisor")]
use nauti_fabric::vmm::{CloudHypervisorAdapter, VmResourceSpec};
#[cfg(feature = "cloud-hypervisor")]
use std::path::PathBuf;
use nauti_fabric::{Fabric, LocalProofAdapter, LocalResourceAdapter, NetworkResourceAdapter, Resource, ResourceKind, ResourceState};
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
    /// Probe local NUMA/PCI topology via hwloc (requires the `numa` build feature).
    #[cfg(feature = "numa")]
    Topology {
        /// Emit the topology report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Probe local NVIDIA GPUs via NVML (requires the `nvidia` build feature).
    #[cfg(feature = "nvidia")]
    Gpus {
        /// Emit the GPU report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Manage local VMs through the Cloud Hypervisor adapter (requires the
    /// `cloud-hypervisor` build feature and the `cloud-hypervisor` + `ch-remote`
    /// binaries on PATH).
    #[cfg(feature = "cloud-hypervisor")]
    Vm {
        #[command(subcommand)]
        action: VmAction,
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
        Command::Inventory { json, node } => inventory(&node, json),
        Command::Demo => demo(),
        Command::AgentServe { node } => agent_serve(&node),
        Command::AgentConnect { addr } => agent_connect(&addr),
        #[cfg(feature = "numa")]
        Command::Topology { json } => topology(json),
        #[cfg(feature = "nvidia")]
        Command::Gpus { json } => gpus(json),
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
                ttl_secs,
            }),
            VmAction::Reconcile => vm_reconcile(),
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

#[cfg(feature = "nvidia")]
fn gpus(json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let topology = nauti_fabric::gpu::GpuTopology::discover()?;

    if json {
        println!("{}", serde_json::to_string_pretty(topology.devices())?);
    } else {
        println!("INDEX\tNAME\tUUID\tTOTAL_BYTES\tFREE_BYTES\tPCI_BUS_ID");
        for gpu in topology.devices() {
            println!(
                "{}\t{}\t{}\t{}\t{}\t{}",
                gpu.index,
                gpu.name,
                gpu.uuid,
                gpu.total_memory_bytes,
                gpu.free_memory_bytes,
                gpu.pci_bus_id
            );
        }
    }

    Ok(())
}

fn inventory(node: &str, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let fabric = Fabric::default();
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
    ttl_secs: u64,
}

#[cfg(feature = "cloud-hypervisor")]
fn vm_capability() -> Result<(), Box<dyn std::error::Error>> {
    // Probe the default binary path; the operator can override via the
    // `CH_BINARY` env var or by setting `--binary` on the `launch` subcommand.
    let default = std::env::var("CH_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/usr/bin/cloud-hypervisor"));
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
