use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use nauti_fabric::{Fabric, LocalProofAdapter, LocalResourceAdapter, NetworkResourceAdapter, Resource, ResourceAdapter, ResourceKind, ResourceState};

#[derive(serde::Serialize)]
struct ToolInfo {
    name: &'static str,
    purpose: &'static str,
}

#[derive(serde::Serialize)]
struct AdapterInfo {
    name: String,
    scope: &'static str,
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
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().command {
        Command::Tools { json } => tools(json),
        Command::Adapters { json } => adapters(json),
        Command::Inventory { json, node } => inventory(&node, json),
        Command::Demo => demo(),
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
    let proof_adapter = LocalProofAdapter;
    let local_adapter = LocalResourceAdapter;
    let network_adapter = NetworkResourceAdapter;
    let adapters = vec![
        AdapterInfo { name: proof_adapter.name().into(), scope: "proof-only" },
        AdapterInfo { name: local_adapter.name().into(), scope: "local-host" },
        AdapterInfo { name: network_adapter.name().into(), scope: "remote-descriptor" },
    ];

    if json {
        println!("{}", serde_json::to_string_pretty(&adapters)?);
    } else {
        println!("NAME\tSCOPE");
        for adapter in adapters {
            println!("{}\t{}", adapter.name, adapter.scope);
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