//! Sysfs-only hardware facts: NUMA nodes, NIC link speed / PCI / NUMA
//! placement, RDMA (RoCE / iWARP / InfiniBand) ports, and GPU NUMA
//! placement. Always compiled, no native deps; the hwloc path stays behind
//! the `numa` feature. Every reader takes the sysfs root so tests can point
//! it at a temp tree.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::{Resource, ResourceKind, ResourceState};

fn read(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

fn entries(dir: impl AsRef<Path>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    out.sort();
    out
}

fn link_name(path: impl AsRef<Path>) -> Option<String> {
    fs::read_link(path)
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
}

/// NUMA node of a PCI device; `-1` from the kernel means "no affinity".
fn pci_numa(device_dir: &Path) -> Option<String> {
    read(device_dir.join("numa_node")).filter(|n| n != "-1")
}

/// `MemTotal` / `MemFree` in bytes from a node's `meminfo`.
fn node_meminfo(node_dir: &Path) -> (u64, u64) {
    let mut total = 0;
    let mut free = 0;
    for line in read(node_dir.join("meminfo")).unwrap_or_default().lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 4 {
            let kb: u64 = parts[3].parse().unwrap_or(0);
            match parts[2] {
                "MemTotal:" => total = kb * 1024,
                "MemFree:" => free = kb * 1024,
                _ => {}
            }
        }
    }
    (total, free)
}

/// One `local.numa.<n>` Memory resource per NUMA node.
pub fn numa_resources(sys: &Path, node: &str) -> Vec<Resource> {
    entries(sys.join("devices/system/node"))
        .into_iter()
        .filter_map(|dir| {
            let name = dir.file_name()?.to_string_lossy().into_owned();
            let index = name.strip_prefix("node")?.parse::<u32>().ok()?;
            let (total, free) = node_meminfo(&dir);
            Some(Resource {
                id: format!("local.numa.{index}"),
                kind: ResourceKind::Memory,
                capacity: total,
                unit: "bytes".into(),
                node: node.to_string(),
                state: ResourceState::Available,
                exclusive: false,
                attributes: BTreeMap::from([
                    ("numa.node".into(), index.to_string()),
                    ("numa.cpulist".into(), read(dir.join("cpulist")).unwrap_or_default()),
                    ("numa.free_bytes".into(), free.to_string()),
                ]),
            })
        })
        .collect()
}

/// Map netdev name -> (rdma device, port) from `/sys/class/infiniband`.
fn rdma_by_netdev(sys: &Path) -> BTreeMap<String, (String, String)> {
    let mut out = BTreeMap::new();
    for dev in entries(sys.join("class/infiniband")) {
        let name = match dev.file_name() {
            Some(n) => n.to_string_lossy().into_owned(),
            None => continue,
        };
        for net in entries(dev.join("device/net")) {
            if let Some(netdev) = net.file_name() {
                out.insert(netdev.to_string_lossy().into_owned(), (name.clone(), "1".into()));
            }
        }
    }
    out
}

/// Add link speed, operstate, driver, PCI BDF, NUMA node, IPv4 and RDMA
/// device to the `local.network.<if>` resources. Capacity becomes bits/s
/// when the kernel reports a speed.
pub fn enrich_network(sys: &Path, resources: &mut [Resource], ipv4: &BTreeMap<String, Vec<String>>) {
    let rdma = rdma_by_netdev(sys);
    for resource in resources.iter_mut().filter(|r| r.kind == ResourceKind::Network) {
        let Some(iface) = resource.attributes.get("interface").cloned() else { continue };
        let dir = sys.join("class/net").join(&iface);
        if !dir.exists() {
            continue;
        }
        let attrs = &mut resource.attributes;
        if let Some(state) = read(dir.join("operstate")) {
            attrs.insert("net.operstate".into(), state);
        }
        if let Some(mbps) = read(dir.join("speed")).and_then(|s| s.parse::<i64>().ok()).filter(|s| *s > 0) {
            attrs.insert("net.speed_mbps".into(), mbps.to_string());
            resource.capacity = mbps as u64 * 1_000_000;
            resource.unit = "bits/s".into();
        }
        if let Some(mtu) = read(dir.join("mtu")) {
            attrs.insert("net.mtu".into(), mtu);
        }
        let device = dir.join("device");
        if device.exists() {
            if let Some(bdf) = fs::canonicalize(&device).ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())) {
                attrs.insert("net.pci_bdf".into(), bdf);
            }
            if let Some(driver) = link_name(device.join("driver")) {
                attrs.insert("net.driver".into(), driver);
            }
            if let Some(numa) = pci_numa(&device) {
                attrs.insert("net.numa_node".into(), numa);
            }
        } else {
            attrs.insert("net.virtual".into(), "true".into());
        }
        if let Some(addrs) = ipv4.get(&iface) {
            attrs.insert("net.ipv4".into(), addrs.join(","));
        }
        if let Some((dev, _)) = rdma.get(&iface) {
            attrs.insert("net.rdma_device".into(), dev.clone());
        }
    }
}

/// One `local.rdma.<dev>.<port>` Network resource per RDMA port, with
/// link layer, state, rate, GID 0, netdev and NUMA placement.
pub fn rdma_resources(sys: &Path, node: &str) -> Vec<Resource> {
    let mut out = Vec::new();
    for dev in entries(sys.join("class/infiniband")) {
        let Some(name) = dev.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
        let device = dev.join("device");
        let netdevs: Vec<String> = entries(device.join("net"))
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        for port in entries(dev.join("ports")) {
            let Some(port_num) = port.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
            let state = read(port.join("state")).unwrap_or_default();
            let active = state.contains("ACTIVE");
            let rate = read(port.join("rate")).unwrap_or_default();
            let bits: u64 = rate
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<f64>().ok())
                .map(|g| (g * 1e9) as u64)
                .unwrap_or(0);
            let mut attrs = BTreeMap::from([
                ("rdma.device".into(), name.clone()),
                ("rdma.port".into(), port_num.clone()),
                ("rdma.state".into(), state.split(':').next_back().unwrap_or("").trim().to_string()),
                ("rdma.rate".into(), rate),
                ("rdma.link_layer".into(), read(port.join("link_layer")).unwrap_or_default()),
                ("rdma.transport".into(), rdma_transport(&name)),
            ]);
            if let Some(netdev) = netdevs.first() {
                attrs.insert("rdma.netdev".into(), netdev.clone());
            }
            if let Some(numa) = pci_numa(&device) {
                attrs.insert("rdma.numa_node".into(), numa);
            }
            if let Some(driver) = link_name(device.join("driver")) {
                attrs.insert("rdma.driver".into(), driver);
            }
            if let Some(gid) = read(port.join("gids/0")) {
                attrs.insert("rdma.gid0".into(), gid);
            }
            out.push(Resource {
                id: format!("local.rdma.{name}.{port_num}"),
                kind: ResourceKind::Network,
                capacity: bits,
                unit: "bits/s".into(),
                node: node.to_string(),
                state: if active { ResourceState::Available } else { ResourceState::Unavailable },
                // A lease reserves the link for one job, like a GPU.
                exclusive: true,
                attributes: attrs,
            });
        }
    }
    out
}

fn rdma_transport(name: &str) -> String {
    if name.starts_with("roce") || name.starts_with("bnxt_re") || name.starts_with("mlx") {
        "roce".into()
    } else if name.starts_with("iw") || name.starts_with("irdma") {
        "iwarp".into()
    } else if name.starts_with("ib") || name.starts_with("hfi") {
        "infiniband".into()
    } else {
        "unknown".into()
    }
}

/// Add `gpu.numa_node` from the GPU's PCI device.
pub fn enrich_gpu_numa(sys: &Path, resources: &mut [Resource]) {
    for resource in resources.iter_mut().filter(|r| r.kind == ResourceKind::Gpu) {
        let Some(bdf) = resource.attributes.get("gpu.pci_bdf").cloned() else { continue };
        if let Some(numa) = pci_numa(&sys.join("bus/pci/devices").join(&bdf)) {
            resource.attributes.insert("gpu.numa_node".into(), numa);
        }
    }
}

/// Interface -> IPv4/prefix list from `ip -4 -o addr` (empty if `ip` is missing).
pub fn ipv4_by_interface() -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let Ok(output) = std::process::Command::new("ip").args(["-4", "-o", "addr", "show"]).output() else {
        return out;
    };
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if let (Some(iface), Some(pos)) = (parts.get(1), parts.iter().position(|p| *p == "inet")) {
            if let Some(addr) = parts.get(pos + 1) {
                out.entry(iface.trim_end_matches(':').to_string()).or_default().push(addr.to_string());
            }
        }
    }
    out
}

/// Everything above against the live `/sys`.
pub fn discover_hardware(node: &str, resources: &mut Vec<Resource>) {
    let sys = Path::new("/sys");
    enrich_network(sys, resources, &ipv4_by_interface());
    enrich_gpu_numa(sys, resources);
    resources.extend(numa_resources(sys, node));
    resources.extend(rdma_resources(sys, node));
}

/// Lease/attach an RDMA port for a job. Attach re-reads sysfs so a port that
/// went DOWN after inventory is refused, and hands back what an ibverbs /
/// UCX / perftest job needs: device, port, RoCE v2 GID index, netdev, IPv4.
pub struct RdmaAdapter {
    sys: PathBuf,
}

impl Default for RdmaAdapter {
    fn default() -> Self {
        Self { sys: PathBuf::from("/sys") }
    }
}

impl RdmaAdapter {
    pub fn with_sys_root(sys: impl Into<PathBuf>) -> Self {
        Self { sys: sys.into() }
    }

    /// First GID index whose type is RoCE v2 and whose GID is IPv4-mapped.
    fn roce_v2_gid_index(&self, device: &str, port: &str) -> Option<String> {
        let port_dir = self.sys.join("class/infiniband").join(device).join("ports").join(port);
        let mut indices: Vec<u32> = entries(port_dir.join("gids"))
            .iter()
            .filter_map(|p| p.file_name()?.to_str()?.parse().ok())
            .collect();
        indices.sort();
        indices.into_iter().map(|i| i.to_string()).find(|i| {
            let gid = read(port_dir.join("gids").join(i)).unwrap_or_default();
            let kind = read(port_dir.join("gid_attrs/types").join(i)).unwrap_or_default();
            kind == "RoCE v2" && gid.starts_with("0000:0000:0000:0000:0000:ffff:")
        })
    }
}

impl crate::ResourceAdapter for RdmaAdapter {
    fn name(&self) -> &str {
        "rdma"
    }

    fn attach(&self, resource: &Resource, lease: &crate::Lease) -> Result<crate::Attachment, crate::FabricError> {
        if !resource.id.starts_with("local.rdma.") {
            return Err(crate::FabricError::MissingResourceAttribute("rdma.device".into()));
        }
        let device = resource
            .attributes
            .get("rdma.device")
            .cloned()
            .ok_or_else(|| crate::FabricError::MissingResourceAttribute("rdma.device".into()))?;
        let port = resource.attributes.get("rdma.port").cloned().unwrap_or_else(|| "1".into());
        let live = rdma_resources(&self.sys, &resource.node)
            .into_iter()
            .find(|r| r.id == resource.id)
            .ok_or_else(|| crate::FabricError::AdapterBackendUnavailable {
                adapter: "rdma".into(),
                reason: format!("{device} port {port} no longer in /sys/class/infiniband"),
            })?;
        if live.state != ResourceState::Available {
            return Err(crate::FabricError::AdapterBackendUnavailable {
                adapter: "rdma".into(),
                reason: format!("{device} port {port} is {}", live.attributes["rdma.state"]),
            });
        }
        let mut details = live.attributes.clone();
        details.insert("attachment.scope".into(), "rdma-port".into());
        if let Some(idx) = self.roce_v2_gid_index(&device, &port) {
            details.insert("rdma.roce_v2_gid_index".into(), idx);
        }
        if let Some(netdev) = live.attributes.get("rdma.netdev") {
            if let Some(ips) = ipv4_by_interface().get(netdev) {
                details.insert("rdma.ipv4".into(), ips.join(","));
            }
        }
        Ok(crate::Attachment {
            resource_id: resource.id.clone(),
            lease_id: lease.id,
            adapter: "rdma".into(),
            details,
        })
    }

    fn capability_report(&self) -> crate::AdapterReport {
        let ports = rdma_resources(&self.sys, "local");
        let active: Vec<String> = ports
            .iter()
            .filter(|r| r.state == ResourceState::Available)
            .map(|r| format!("{}:{}", r.attributes["rdma.device"], r.attributes["rdma.rate"]))
            .collect();
        crate::AdapterReport {
            name: "rdma".into(),
            scope: "rdma-roce-iwarp-ib".into(),
            healthy: !active.is_empty(),
            detail: BTreeMap::from([
                ("rdma.ports".into(), ports.len().to_string()),
                ("rdma.active".into(), if active.is_empty() { "none".into() } else { active.join(",") }),
            ]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ResourceAdapter;
    use std::os::unix::fs::symlink;

    fn write(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn fake_sys() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        let s = t.path();
        write(&s.join("devices/system/node/node0/cpulist"), "0-23\n");
        write(
            &s.join("devices/system/node/node0/meminfo"),
            "Node 0 MemTotal:       1048576 kB\nNode 0 MemFree:         524288 kB\n",
        );
        write(&s.join("devices/system/node/node1/cpulist"), "24-47\n");
        write(&s.join("devices/system/node/node1/meminfo"), "Node 1 MemTotal:       2097152 kB\n");
        write(&s.join("devices/system/node/possible"), "0-1\n");

        let pci = s.join("devices/pci0000:00/0000:01:00.0");
        write(&pci.join("numa_node"), "1\n");
        fs::create_dir_all(s.join("bus/pci/drivers/bnxt_en")).unwrap();
        symlink(s.join("bus/pci/drivers/bnxt_en"), pci.join("driver")).unwrap();
        fs::create_dir_all(s.join("bus/pci/devices")).unwrap();
        symlink(&pci, s.join("bus/pci/devices/0000:01:00.0")).unwrap();

        let net = s.join("class/net/enp1s0f0np0");
        write(&net.join("operstate"), "up\n");
        write(&net.join("speed"), "25000\n");
        write(&net.join("mtu"), "9000\n");
        symlink(&pci, net.join("device")).unwrap();
        let virt = s.join("class/net/docker0");
        write(&virt.join("operstate"), "down\n");
        write(&virt.join("speed"), "-1\n");

        let ib = s.join("class/infiniband/rocep1s0f0");
        fs::create_dir_all(pci.join("net/enp1s0f0np0")).unwrap();
        fs::create_dir_all(&ib).unwrap();
        symlink(&pci, ib.join("device")).unwrap();
        write(&ib.join("ports/1/state"), "4: ACTIVE\n");
        write(&ib.join("ports/1/rate"), "25 Gb/sec (1X EDR)\n");
        write(&ib.join("ports/1/link_layer"), "Ethernet\n");
        write(&ib.join("ports/1/gids/0"), "fe80:0000:0000:0000:b026:28ff:fe3d:22d0\n");
        t
    }

    fn net_resource(iface: &str) -> Resource {
        Resource {
            id: format!("local.network.{iface}"),
            kind: ResourceKind::Network,
            capacity: 0,
            unit: "unknown-bandwidth".into(),
            node: "n".into(),
            state: ResourceState::Available,
            exclusive: false,
            attributes: BTreeMap::from([("interface".into(), iface.to_string())]),
        }
    }

    #[test]
    fn numa_nodes_become_memory_resources() {
        let t = fake_sys();
        let numa = numa_resources(t.path(), "n");
        assert_eq!(numa.len(), 2);
        assert_eq!(numa[0].id, "local.numa.0");
        assert_eq!(numa[0].capacity, 1024 * 1024 * 1024);
        assert_eq!(numa[0].attributes["numa.cpulist"], "0-23");
        assert_eq!(numa[0].attributes["numa.free_bytes"], (512 * 1024 * 1024u64).to_string());
        assert_eq!(numa[1].attributes["numa.node"], "1");
    }

    #[test]
    fn network_gets_speed_pci_numa_and_rdma() {
        let t = fake_sys();
        let mut res = vec![net_resource("enp1s0f0np0"), net_resource("docker0")];
        let ips = BTreeMap::from([("enp1s0f0np0".to_string(), vec!["10.0.10.61/24".to_string()])]);
        enrich_network(t.path(), &mut res, &ips);
        let a = &res[0].attributes;
        assert_eq!(res[0].capacity, 25_000_000_000);
        assert_eq!(res[0].unit, "bits/s");
        assert_eq!(a["net.speed_mbps"], "25000");
        assert_eq!(a["net.pci_bdf"], "0000:01:00.0");
        assert_eq!(a["net.driver"], "bnxt_en");
        assert_eq!(a["net.numa_node"], "1");
        assert_eq!(a["net.ipv4"], "10.0.10.61/24");
        assert_eq!(a["net.rdma_device"], "rocep1s0f0");
        let v = &res[1];
        assert_eq!(v.capacity, 0, "speed -1 must not become capacity");
        assert_eq!(v.attributes["net.virtual"], "true");
    }

    #[test]
    fn rdma_ports_are_typed_resources() {
        let t = fake_sys();
        let rdma = rdma_resources(t.path(), "n");
        assert_eq!(rdma.len(), 1);
        let r = &rdma[0];
        assert_eq!(r.id, "local.rdma.rocep1s0f0.1");
        assert_eq!(r.capacity, 25_000_000_000);
        assert_eq!(r.state, ResourceState::Available);
        assert!(r.exclusive, "rdma ports are leased like GPUs");
        assert_eq!(r.attributes["rdma.state"], "ACTIVE");
        assert_eq!(r.attributes["rdma.transport"], "roce");
        assert_eq!(r.attributes["rdma.link_layer"], "Ethernet");
        assert_eq!(r.attributes["rdma.netdev"], "enp1s0f0np0");
        assert_eq!(r.attributes["rdma.numa_node"], "1");
    }

    #[test]
    fn down_rdma_port_is_unavailable() {
        let t = fake_sys();
        write(&t.path().join("class/infiniband/rocep1s0f0/ports/1/state"), "1: DOWN\n");
        write(&t.path().join("class/infiniband/rocep1s0f0/ports/1/rate"), "2.5 Gb/sec (1X SDR)\n");
        let rdma = rdma_resources(t.path(), "n");
        assert_eq!(rdma[0].state, ResourceState::Unavailable);
        assert_eq!(rdma[0].capacity, 2_500_000_000);
        assert_eq!(rdma[0].attributes["rdma.state"], "DOWN");
    }

    #[test]
    fn gpu_numa_from_pci_device() {
        let t = fake_sys();
        let mut res = vec![Resource {
            id: "local.gpu.x".into(),
            kind: ResourceKind::Gpu,
            capacity: 1,
            unit: "bytes".into(),
            node: "n".into(),
            state: ResourceState::Available,
            exclusive: true,
            attributes: BTreeMap::from([("gpu.pci_bdf".into(), "0000:01:00.0".into())]),
        }];
        enrich_gpu_numa(t.path(), &mut res);
        assert_eq!(res[0].attributes["gpu.numa_node"], "1");
    }

    fn lease_for(id: &str) -> crate::Lease {
        crate::Lease {
            id: 7,
            resource_id: id.into(),
            owner: "test".into(),
        }
    }

    #[test]
    fn rdma_adapter_attaches_active_port_with_roce_v2_gid() {
        let t = fake_sys();
        let port = t.path().join("class/infiniband/rocep1s0f0/ports/1");
        write(&port.join("gids/3"), "0000:0000:0000:0000:0000:ffff:0a00:0a3d\n");
        write(&port.join("gid_attrs/types/0"), "IB/RoCE v1\n");
        write(&port.join("gid_attrs/types/3"), "RoCE v2\n");
        let adapter = RdmaAdapter::with_sys_root(t.path());
        let res = rdma_resources(t.path(), "n").remove(0);
        let att = adapter.attach(&res, &lease_for(&res.id)).unwrap();
        assert_eq!(att.adapter, "rdma");
        assert_eq!(att.details["rdma.roce_v2_gid_index"], "3");
        assert_eq!(att.details["attachment.scope"], "rdma-port");
        assert!(adapter.capability_report().healthy);
    }

    #[test]
    fn rdma_adapter_refuses_port_that_went_down() {
        let t = fake_sys();
        let res = rdma_resources(t.path(), "n").remove(0);
        write(&t.path().join("class/infiniband/rocep1s0f0/ports/1/state"), "1: DOWN\n");
        let adapter = RdmaAdapter::with_sys_root(t.path());
        let err = adapter.attach(&res, &lease_for(&res.id)).unwrap_err();
        assert!(matches!(err, crate::FabricError::AdapterBackendUnavailable { .. }));
        assert!(!adapter.capability_report().healthy);
    }

    #[test]
    fn rdma_adapter_rejects_non_rdma_resource() {
        let t = fake_sys();
        let adapter = RdmaAdapter::with_sys_root(t.path());
        let res = net_resource("enp1s0f0np0");
        assert!(adapter.attach(&res, &lease_for(&res.id)).is_err());
    }

    #[test]
    fn transport_names() {
        assert_eq!(rdma_transport("rocep134s0f0"), "roce");
        assert_eq!(rdma_transport("iwo1"), "iwarp");
        assert_eq!(rdma_transport("mlx5_0"), "roce");
        assert_eq!(rdma_transport("ibp1s0"), "infiniband");
    }
}
