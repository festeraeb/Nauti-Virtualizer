use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use nauti_fabric::rpc::{AgentClient, RpcRequest, RpcResponse};
use nauti_fabric::{Fabric, LocalProofAdapter, LocalResourceAdapter, NetworkResourceAdapter, Resource, ResourceKind, ResourceState};

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