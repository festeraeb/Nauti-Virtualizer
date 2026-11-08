//! Smart routing for bulk bytes: find what two machines can do together and use the best of it.
//!
//! The mesh (Nexus) decides who may talk; this module decides how the bytes go. Each node describes
//! itself ([`Capabilities`]): its physical links with their speed and IPv4 subnets, any RDMA port on a
//! link and the RoCE v2 GID that carries that link's address, how much memory it could pin to serve
//! (MemAvailable and the memory-lock limit), and the NFS exports it has mounted. [`select`] takes a
//! client's and a server's capabilities and ranks every path they share:
//!
//! - **roce**: both ends have an active RoCE port on a shared subnet and the server can pin what is asked.
//! - **nfs-mmap**: the client has an export of the server mounted.
//! - **tcp**: a shared subnet, at the slower end's link speed.
//! - **mesh**: always possible between members, and last: measured at 12-16 MB/s through lemond's relay.
//!
//! Nothing here is specific to our fleet. A machine without RDMA gets tcp, one with it gets roce, and a
//! pair with no shared link gets the mesh, each with the reason, so anyone who has the hardware uses it.
//! Estimates are ratios of the slower link, from measurements on 2026-11-07 (RoCE at the line rate,
//! cold NFS-mmap and one TCP stream about 1.0-1.3 GB/s on 25 GbE); a measured number replaces an estimate
//! through [`Plan::with_measured`].

use std::collections::BTreeMap;
use std::fs;
use std::net::Ipv4Addr;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// An RDMA port on a link, as the kernel reports it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RdmaPort {
    pub device: String,
    pub port: u32,
    pub active: bool,
    /// "Ethernet" for RoCE, "InfiniBand" for IB.
    pub link_layer: String,
    /// The GID index whose type is RoCE v2 and whose address is this link's IPv4: what a queue pair on
    /// this link uses. None when the port has no such GID (no IPv4 on the netdev, or RoCE v1 only).
    pub roce_v2_gid_index: Option<u32>,
}

/// One physical network link.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Link {
    pub iface: String,
    /// "a.b.c.d/len" entries.
    pub ipv4: Vec<String>,
    pub speed_mbps: Option<u64>,
    pub up: bool,
    pub rdma: Option<RdmaPort>,
}

/// An NFS export this node has mounted.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NfsMount {
    /// The server part of the source ("host" of "host:/export").
    pub server: String,
    pub export: String,
    pub mountpoint: String,
}

/// What one node can do for bulk transfers.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Capabilities {
    pub node: String,
    pub links: Vec<Link>,
    pub mem_available_bytes: u64,
    /// The memory-lock limit of the process that reported this; None means unlimited.
    pub memlock_bytes: Option<u64>,
    pub nfs_mounts: Vec<NfsMount>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PathKind {
    Roce,
    NfsMmap,
    Tcp,
    Mesh,
}

/// One way the bytes could go.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PathChoice {
    pub kind: PathKind,
    pub client_iface: Option<String>,
    pub server_iface: Option<String>,
    /// The server address the client connects to (or the mount, for nfs-mmap).
    pub server_addr: Option<String>,
    pub client_gid_index: Option<u32>,
    pub server_gid_index: Option<u32>,
    /// Estimated (or, after [`Plan::with_measured`], measured) throughput.
    pub mb_per_s: u64,
    pub measured: bool,
    pub reason: String,
}

/// What the transfer needs from the server.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Need {
    /// Bytes the server must pin for RDMA (its registered window).
    pub pinned_bytes: u64,
    /// The file as the client sees it, when known: only the NFS mount that holds it is a path.
    pub client_path: Option<String>,
}

/// The ranked paths for one client/server pair, best first, and why others were left out.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Plan {
    pub client: String,
    pub server: String,
    pub paths: Vec<PathChoice>,
    pub skipped: Vec<String>,
}

impl Plan {
    /// The best path. There is always one: the mesh.
    pub fn best(&self) -> &PathChoice {
        &self.paths[0]
    }

    /// Replace estimates with measured throughput, keyed "kind" or "kind@client_iface", and re-rank.
    pub fn with_measured(mut self, measured: &BTreeMap<String, u64>) -> Self {
        for p in &mut self.paths {
            let kind = serde_json::to_value(p.kind).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
            let keyed = p.client_iface.as_ref().map(|i| format!("{kind}@{i}"));
            if let Some(mb) = keyed.as_ref().and_then(|k| measured.get(k)).or_else(|| measured.get(&kind)) {
                p.mb_per_s = *mb;
                p.measured = true;
            }
        }
        rank(&mut self.paths);
        self
    }
}

/// The mesh relay, measured c1 -> c2 on #3930 (26bdfb48f): 12-16 MB/s.
pub const MESH_MB_PER_S: u64 = 14;
/// One TCP stream and a cold NFS-mmap read top out near these on 25 GbE (2026-11-07, c4 <- t440).
const TCP_CAP_MB_PER_S: u64 = 1_000;
const NFS_CAP_MB_PER_S: u64 = 1_300;

fn line_mb_per_s(mbps: u64) -> u64 {
    mbps / 8 * 9 / 10
}

fn parse_cidr(cidr: &str) -> Option<(Ipv4Addr, u8)> {
    let (addr, len) = cidr.split_once('/').unwrap_or((cidr, "32"));
    Some((addr.parse().ok()?, len.parse().ok().filter(|l: &u8| *l <= 32)?))
}

fn same_subnet(a: &str, b: &str) -> Option<String> {
    let (ia, la) = parse_cidr(a)?;
    let (ib, _) = parse_cidr(b)?;
    let mask = if la == 0 { 0 } else { u32::MAX << (32 - la) };
    (u32::from(ia) & mask == u32::from(ib) & mask).then(|| ib.to_string())
}

/// Every path `client` and `server` share, best first.
pub fn select(client: &Capabilities, server: &Capabilities, need: &Need) -> Plan {
    let mut paths = Vec::new();
    let mut skipped = Vec::new();
    for cl in client.links.iter().filter(|l| l.up) {
        for sl in server.links.iter().filter(|l| l.up) {
            let Some(addr) = cl.ipv4.iter().find_map(|c| sl.ipv4.iter().find_map(|s| same_subnet(c, s))) else { continue };
            let speed = match (cl.speed_mbps, sl.speed_mbps) {
                (Some(a), Some(b)) => a.min(b),
                (Some(a), None) | (None, Some(a)) => a,
                (None, None) => 1_000,
            };
            let line = line_mb_per_s(speed);
            let via = format!("{} -> {} ({addr}), {speed} Mb/s", cl.iface, sl.iface);
            match (&cl.rdma, &sl.rdma) {
                (Some(cr), Some(sr)) => {
                    let ethernet = cr.link_layer.eq_ignore_ascii_case("ethernet") && sr.link_layer.eq_ignore_ascii_case("ethernet");
                    let pinnable = server.memlock_bytes.map_or(u64::MAX, |m| m).min(server.mem_available_bytes);
                    let why_not = if !cr.active || !sr.active {
                        Some("an RDMA port is not active")
                    } else if !ethernet {
                        Some("not RoCE (link layer is not Ethernet on both ends)")
                    } else if cr.roce_v2_gid_index.is_none() || sr.roce_v2_gid_index.is_none() {
                        Some("no RoCE v2 GID for this link's IPv4 on both ends")
                    } else if need.pinned_bytes > pinnable {
                        Some("the server cannot pin that much (memory-lock limit or free memory)")
                    } else {
                        None
                    };
                    match why_not {
                        None => paths.push(PathChoice {
                            kind: PathKind::Roce,
                            client_iface: Some(cl.iface.clone()),
                            server_iface: Some(sl.iface.clone()),
                            server_addr: Some(addr.clone()),
                            client_gid_index: cr.roce_v2_gid_index,
                            server_gid_index: sr.roce_v2_gid_index,
                            mb_per_s: line,
                            measured: false,
                            reason: format!("RoCE v2 {} port {} -> {} port {}, {via}", cr.device, cr.port, sr.device, sr.port),
                        }),
                        Some(why) => skipped.push(format!("roce on {via}: {why}")),
                    }
                }
                (None, None) => skipped.push(format!("roce on {via}: no RDMA on either end")),
                (None, Some(_)) => skipped.push(format!("roce on {via}: no RDMA on {}", client.node)),
                (Some(_), None) => skipped.push(format!("roce on {via}: no RDMA on {}", server.node)),
            }
            let server_names: Vec<String> = std::iter::once(server.node.clone())
                .chain(sl.ipv4.iter().filter_map(|c| parse_cidr(c).map(|(a, _)| a.to_string())))
                .collect();
            // The server's exports this client has mounted; with a file named, only the mount that holds it.
            let mounts: Vec<&NfsMount> = client
                .nfs_mounts
                .iter()
                .filter(|m| server_names.iter().any(|n| n == &m.server))
                .filter(|m| need.client_path.as_deref().is_none_or(|f| Path::new(f).starts_with(&m.mountpoint)))
                .collect();
            if !mounts.is_empty() {
                let at: Vec<&str> = mounts.iter().map(|m| m.mountpoint.as_str()).collect();
                paths.push(PathChoice {
                    kind: PathKind::NfsMmap,
                    client_iface: Some(cl.iface.clone()),
                    server_iface: Some(sl.iface.clone()),
                    server_addr: Some(at.join(",")),
                    client_gid_index: None,
                    server_gid_index: None,
                    mb_per_s: line.min(NFS_CAP_MB_PER_S),
                    measured: false,
                    reason: format!("NFS mounted at {}, {via}", at.join(", ")),
                });
            } else if need.client_path.is_some() && client.nfs_mounts.iter().any(|m| server_names.iter().any(|n| n == &m.server)) {
                skipped.push(format!("nfs-mmap on {via}: no mount of {} holds that file", server.node));
            }
            paths.push(PathChoice {
                kind: PathKind::Tcp,
                client_iface: Some(cl.iface.clone()),
                server_iface: Some(sl.iface.clone()),
                server_addr: Some(addr),
                client_gid_index: None,
                server_gid_index: None,
                mb_per_s: line.min(TCP_CAP_MB_PER_S),
                measured: false,
                reason: format!("direct TCP, {via}"),
            });
        }
    }
    if paths.is_empty() {
        skipped.push(format!("no shared subnet between {} and {}", client.node, server.node));
    }
    paths.push(PathChoice {
        kind: PathKind::Mesh,
        client_iface: None,
        server_iface: None,
        server_addr: None,
        client_gid_index: None,
        server_gid_index: None,
        mb_per_s: MESH_MB_PER_S,
        measured: false,
        reason: "the Nexus mesh relay: always available between members, never for bulk when anything else is".into(),
    });
    rank(&mut paths);
    paths.dedup_by(|a, b| a.kind == b.kind && a.client_iface == b.client_iface && a.server_addr == b.server_addr);
    Plan { client: client.node.clone(), server: server.node.clone(), paths, skipped }
}

/// Fastest first; on a tie the better kind (roce, nfs-mmap, tcp, mesh). The mesh always stays last.
fn rank(paths: &mut [PathChoice]) {
    paths.sort_by(|a, b| {
        (a.kind == PathKind::Mesh)
            .cmp(&(b.kind == PathKind::Mesh))
            .then(b.mb_per_s.cmp(&a.mb_per_s))
            .then(a.kind.cmp(&b.kind))
    });
}

// ---------- Discovery ----------

fn read(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

fn names(dir: impl AsRef<Path>) -> Vec<String> {
    let mut out: Vec<String> = fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    out.sort();
    out
}

/// The IPv4 address in a RoCE v2 GID ("0000:0000:0000:0000:0000:ffff:0a00:0a3d" -> 10.0.10.61).
fn gid_ipv4(gid: &str) -> Option<Ipv4Addr> {
    let groups: Vec<&str> = gid.split(':').collect();
    if groups.len() != 8 || groups[..5].iter().any(|g| u16::from_str_radix(g, 16).ok() != Some(0)) || !groups[5].eq_ignore_ascii_case("ffff") {
        return None;
    }
    let hi = u16::from_str_radix(groups[6], 16).ok()?;
    let lo = u16::from_str_radix(groups[7], 16).ok()?;
    Some(Ipv4Addr::new((hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8))
}

/// RDMA ports by netdev, each with the RoCE v2 GID index for each of the netdev's IPv4 addresses.
fn rdma_ports(sys: &Path) -> BTreeMap<String, (RdmaPort, BTreeMap<Ipv4Addr, u32>)> {
    let mut out = BTreeMap::new();
    let ib = sys.join("class/infiniband");
    for dev in names(&ib) {
        for port in names(ib.join(&dev).join("ports")) {
            let pdir = ib.join(&dev).join("ports").join(&port);
            let Ok(port_num) = port.parse::<u32>() else { continue };
            let active = read(pdir.join("state")).is_some_and(|s| s.contains("ACTIVE"));
            let link_layer = read(pdir.join("link_layer")).unwrap_or_default();
            let mut by_ndev: BTreeMap<String, BTreeMap<Ipv4Addr, u32>> = BTreeMap::new();
            for idx in names(pdir.join("gids")) {
                let Ok(i) = idx.parse::<u32>() else { continue };
                let (Some(gid), Some(kind), Some(ndev)) = (
                    read(pdir.join("gids").join(&idx)),
                    read(pdir.join("gid_attrs/types").join(&idx)),
                    read(pdir.join("gid_attrs/ndevs").join(&idx)),
                ) else { continue };
                if !kind.eq_ignore_ascii_case("RoCE v2") {
                    continue;
                }
                if let Some(ip) = gid_ipv4(&gid) {
                    by_ndev.entry(ndev).or_default().entry(ip).or_insert(i);
                }
            }
            // A port whose netdev has no IPv4 GIDs yet still counts as the netdev's RDMA port.
            let netdevs = names(ib.join(&dev).join("device/net"));
            for ndev in netdevs.iter().chain(by_ndev.keys()).cloned().collect::<std::collections::BTreeSet<_>>() {
                let gids = by_ndev.get(&ndev).cloned().unwrap_or_default();
                out.entry(ndev).or_insert((
                    RdmaPort { device: dev.clone(), port: port_num, active, link_layer: link_layer.clone(), roce_v2_gid_index: None },
                    gids,
                ));
            }
        }
    }
    out
}

fn meminfo_available(proc_root: &Path) -> u64 {
    read(proc_root.join("meminfo"))
        .and_then(|m| m.lines().find(|l| l.starts_with("MemAvailable:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|kb| kb.parse::<u64>().ok()))
        .map_or(0, |kb| kb * 1024)
}

fn memlock_limit(proc_root: &Path) -> Option<u64> {
    let limits = read(proc_root.join("self/limits"))?;
    let line = limits.lines().find(|l| l.starts_with("Max locked memory"))?;
    let soft = line.split_whitespace().nth(3)?;
    if soft == "unlimited" { None } else { soft.parse().ok() }
}

fn nfs_mounts(proc_root: &Path) -> Vec<NfsMount> {
    read(proc_root.join("mounts"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 3 || !f[2].starts_with("nfs") {
                return None;
            }
            let (server, export) = f[0].split_once(':')?;
            Some(NfsMount { server: server.trim_matches(['[', ']']).to_string(), export: export.to_string(), mountpoint: f[1].to_string() })
        })
        .collect()
}

impl Capabilities {
    /// This machine, from the live `/sys` and `/proc`.
    pub fn discover(node: &str) -> Self {
        Self::discover_from(node, Path::new("/sys"), Path::new("/proc"), &crate::hwinfo::ipv4_by_interface())
    }

    /// The same from given roots (tests use a fixture tree).
    pub fn discover_from(node: &str, sys: &Path, proc_root: &Path, ipv4: &BTreeMap<String, Vec<String>>) -> Self {
        let rdma = rdma_ports(sys);
        let mut links = Vec::new();
        for iface in names(sys.join("class/net")) {
            let dir = sys.join("class/net").join(&iface);
            if !dir.join("device").exists() {
                continue; // loopback, bridges, tunnels: not a path for bulk bytes
            }
            let addrs = ipv4.get(&iface).cloned().unwrap_or_default();
            let rdma_port = rdma.get(&iface).map(|(p, gids)| {
                let mut p = p.clone();
                p.roce_v2_gid_index = addrs.iter().filter_map(|c| parse_cidr(c)).find_map(|(ip, _)| gids.get(&ip).copied());
                p
            });
            links.push(Link {
                up: read(dir.join("operstate")).is_some_and(|s| s == "up"),
                speed_mbps: read(dir.join("speed")).and_then(|s| s.parse::<i64>().ok()).filter(|s| *s > 0).map(|s| s as u64),
                ipv4: addrs,
                rdma: rdma_port,
                iface,
            });
        }
        Self {
            node: node.to_string(),
            links,
            mem_available_bytes: meminfo_available(proc_root),
            memlock_bytes: memlock_limit(proc_root),
            nfs_mounts: nfs_mounts(proc_root),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn link(iface: &str, ip: &str, mbps: u64, rdma: Option<(&str, Option<u32>)>) -> Link {
        Link {
            iface: iface.into(),
            ipv4: vec![ip.into()],
            speed_mbps: Some(mbps),
            up: true,
            rdma: rdma.map(|(dev, gid)| RdmaPort { device: dev.into(), port: 1, active: true, link_layer: "Ethernet".into(), roce_v2_gid_index: gid }),
        }
    }

    fn node(name: &str, links: Vec<Link>, mem_gib: u64, memlock: Option<u64>) -> Capabilities {
        Capabilities { node: name.into(), links, mem_available_bytes: mem_gib * GIB, memlock_bytes: memlock, nfs_mounts: vec![] }
    }

    // c4 today: 1G LAN, 25G RoCE to t440 (port 1), port 0 free.
    fn c4() -> Capabilities {
        node("c4", vec![
            link("eno1np0", "10.44.0.14/24", 1_000, None),
            link("ens6f1np1", "10.0.10.204/24", 25_000, Some(("rocep135s0f1", Some(5)))),
            link("ens6f0np0", "10.0.11.204/24", 25_000, Some(("rocep135s0f0", Some(3)))),
        ], 60, None)
    }

    #[test]
    fn rdma_wins_where_both_ends_have_it() {
        let t440 = node("t440", vec![
            link("eno1", "10.44.0.10/24", 1_000, None),
            link("ens1f1np1", "10.0.10.61/24", 25_000, Some(("rocep1s0f1", Some(3)))),
        ], 20, None);
        let plan = select(&c4(), &t440, &Need { pinned_bytes: 8 * GIB, ..Default::default() });
        let best = plan.best();
        assert_eq!(best.kind, PathKind::Roce);
        assert_eq!(best.server_addr.as_deref(), Some("10.0.10.61"));
        assert_eq!((best.client_gid_index, best.server_gid_index), (Some(5), Some(3)));
        assert_eq!(plan.paths.last().unwrap().kind, PathKind::Mesh);
    }

    #[test]
    fn c1_today_gets_tcp_on_1g_and_rdma_once_its_card_is_in() {
        let c1_now = node("c1", vec![link("eno1", "10.44.0.11/24", 1_000, None)], 96, None);
        let plan = select(&c4(), &c1_now, &Need { pinned_bytes: 80 * GIB, ..Default::default() });
        assert_eq!(plan.best().kind, PathKind::Tcp);
        assert_eq!(plan.best().mb_per_s, 112);
        assert!(plan.skipped.iter().any(|s| s.contains("no RDMA on either end")));

        let c1_card = node("c1", vec![
            link("eno1", "10.44.0.11/24", 1_000, None),
            link("ens2f0np0", "10.0.11.174/24", 25_000, Some(("rocep2s0f0", Some(3)))),
        ], 96, None);
        let plan = select(&c4(), &c1_card, &Need { pinned_bytes: 80 * GIB, ..Default::default() });
        assert_eq!(plan.best().kind, PathKind::Roce);
        assert_eq!(plan.best().client_iface.as_deref(), Some("ens6f0np0"));
        assert_eq!(plan.best().mb_per_s, 2_812);
    }

    #[test]
    fn a_fast_card_without_rdma_is_still_used_over_tcp() {
        let intel = node("nas", vec![link("enp1s0", "10.0.11.50/24", 25_000, None)], 32, None);
        let plan = select(&c4(), &intel, &Need::default());
        assert_eq!(plan.best().kind, PathKind::Tcp);
        assert_eq!(plan.best().mb_per_s, TCP_CAP_MB_PER_S);
        assert!(plan.skipped.iter().any(|s| s.contains("no RDMA on nas")));
    }

    #[test]
    fn a_server_that_cannot_pin_enough_falls_back_with_the_reason() {
        let tight = node("c1", vec![link("ens2f0np0", "10.0.11.174/24", 25_000, Some(("rocep2s0f0", Some(3))))], 96, Some(64 * 1024));
        let plan = select(&c4(), &tight, &Need { pinned_bytes: 80 * GIB, ..Default::default() });
        assert_eq!(plan.best().kind, PathKind::Tcp);
        assert!(plan.skipped.iter().any(|s| s.contains("cannot pin")));
        let small = select(&c4(), &tight, &Need { pinned_bytes: 32 * 1024, ..Default::default() });
        assert_eq!(small.best().kind, PathKind::Roce);
    }

    #[test]
    fn rdma_without_a_v2_gid_or_inactive_is_skipped() {
        let no_gid = node("x", vec![link("ens2f0np0", "10.0.11.9/24", 25_000, Some(("roce0", None)))], 8, None);
        let plan = select(&c4(), &no_gid, &Need::default());
        assert_eq!(plan.best().kind, PathKind::Tcp);
        assert!(plan.skipped.iter().any(|s| s.contains("RoCE v2 GID")));
    }

    #[test]
    fn no_shared_subnet_leaves_only_the_mesh() {
        let far = node("phone", vec![link("wlan0", "192.168.50.7/24", 400, None)], 4, None);
        let plan = select(&c4(), &far, &Need::default());
        assert_eq!(plan.paths.len(), 1);
        assert_eq!(plan.best().kind, PathKind::Mesh);
        assert!(plan.skipped.iter().any(|s| s.contains("no shared subnet")));
    }

    #[test]
    fn a_mounted_export_offers_nfs_mmap() {
        let mut client = c4();
        client.nfs_mounts.push(NfsMount { server: "10.0.10.61".into(), export: "/mnt/ssd9/models".into(), mountpoint: "/mnt/t440/ssd9-models".into() });
        let t440 = node("t440", vec![link("ens1f1np1", "10.0.10.61/24", 25_000, None)], 20, None);
        client.nfs_mounts.push(NfsMount { server: "10.0.10.61".into(), export: "/home".into(), mountpoint: "/mnt/t440/home".into() });
        let plan = select(&client, &t440, &Need::default());
        assert_eq!(plan.best().kind, PathKind::NfsMmap);
        assert_eq!(plan.paths.iter().filter(|p| p.kind == PathKind::NfsMmap).count(), 1, "mounts of one server are one path");
        assert_eq!(plan.paths[1].kind, PathKind::Tcp);
        let file = Need { client_path: Some("/mnt/t440/ssd9-models/ml4/shard-1.gguf".into()), ..Default::default() };
        let plan = select(&client, &t440, &file);
        assert_eq!(plan.best().server_addr.as_deref(), Some("/mnt/t440/ssd9-models"));
        let elsewhere = Need { client_path: Some("/data/local.gguf".into()), ..Default::default() };
        let plan = select(&client, &t440, &elsewhere);
        assert_eq!(plan.best().kind, PathKind::Tcp);
        assert!(plan.skipped.iter().any(|s| s.contains("no mount of t440 holds that file")));
    }

    #[test]
    fn measured_numbers_replace_estimates_and_re_rank() {
        let t440 = node("t440", vec![link("ens1f1np1", "10.0.10.61/24", 25_000, Some(("rocep1s0f1", Some(3))))], 20, None);
        let plan = select(&c4(), &t440, &Need::default())
            .with_measured(&BTreeMap::from([("roce@ens6f1np1".to_string(), 600), ("tcp".to_string(), 981)]));
        assert_eq!(plan.best().kind, PathKind::Tcp);
        assert!(plan.best().measured);
        assert_eq!(plan.paths.last().unwrap().kind, PathKind::Mesh);
    }

    #[test]
    fn gid_and_cidr_parsing() {
        assert_eq!(gid_ipv4("0000:0000:0000:0000:0000:ffff:0a00:0a3d"), Some(Ipv4Addr::new(10, 0, 10, 61)));
        assert_eq!(gid_ipv4("fe80:0000:0000:0000:0262:0bff:fe0a:1234"), None);
        assert_eq!(same_subnet("10.0.10.204/24", "10.0.10.61/24").as_deref(), Some("10.0.10.61"));
        assert_eq!(same_subnet("10.0.10.204/24", "10.0.11.61/24"), None);
    }

    #[test]
    fn discovery_reads_links_rdma_gids_memory_and_mounts() {
        let root = tempfile::tempdir().unwrap();
        let sys = root.path().join("sys");
        let proc_root = root.path().join("proc");
        let w = |p: &Path, s: &str| { fs::create_dir_all(p.parent().unwrap()).unwrap(); fs::write(p, s).unwrap(); };
        // A 25G RoCE link with an IPv4 GID at index 3, and a 1G link without RDMA.
        let net = sys.join("class/net");
        for (iface, speed) in [("ens6f0np0", "25000"), ("eno1", "1000")] {
            w(&net.join(iface).join("operstate"), "up");
            w(&net.join(iface).join("speed"), speed);
            fs::create_dir_all(net.join(iface).join("device")).unwrap();
        }
        fs::create_dir_all(net.join("lo")).unwrap(); // no device: skipped
        let port = sys.join("class/infiniband/rocep135s0f0/ports/1");
        w(&port.join("state"), "4: ACTIVE");
        w(&port.join("link_layer"), "Ethernet");
        fs::create_dir_all(sys.join("class/infiniband/rocep135s0f0/device/net/ens6f0np0")).unwrap();
        for (idx, gid, kind) in [
            ("0", "fe80:0000:0000:0000:0262:0bff:fe0a:1234", "IB/RoCE v1"),
            ("2", "0000:0000:0000:0000:0000:ffff:0a00:0bcc", "IB/RoCE v1"),
            ("3", "0000:0000:0000:0000:0000:ffff:0a00:0bcc", "RoCE v2"),
        ] {
            w(&port.join("gids").join(idx), gid);
            w(&port.join("gid_attrs/types").join(idx), kind);
            w(&port.join("gid_attrs/ndevs").join(idx), "ens6f0np0");
        }
        w(&proc_root.join("meminfo"), "MemTotal: 100 kB\nMemAvailable:   100663296 kB\n");
        w(&proc_root.join("self/limits"), "Limit Soft Limit Hard Limit Units\nMax locked memory         8388608              8388608              bytes\n");
        w(&proc_root.join("mounts"), "10.0.10.61:/mnt/ssd9/models /mnt/t440/ssd9-models nfs4 rw 0 0\n/dev/sda1 / ext4 rw 0 0\n");
        let ipv4 = BTreeMap::from([("ens6f0np0".to_string(), vec!["10.0.11.204/24".to_string()]), ("eno1".to_string(), vec!["10.44.0.14/24".to_string()])]);
        let caps = Capabilities::discover_from("c4", &sys, &proc_root, &ipv4);
        assert_eq!(caps.links.len(), 2);
        let fast = caps.links.iter().find(|l| l.iface == "ens6f0np0").unwrap();
        let rdma = fast.rdma.as_ref().unwrap();
        assert_eq!((rdma.device.as_str(), rdma.port, rdma.active, rdma.roce_v2_gid_index), ("rocep135s0f0", 1, true, Some(3)));
        assert_eq!(fast.speed_mbps, Some(25_000));
        assert!(caps.links.iter().find(|l| l.iface == "eno1").unwrap().rdma.is_none());
        assert_eq!(caps.mem_available_bytes, 96 * GIB);
        assert_eq!(caps.memlock_bytes, Some(8 * 1024 * 1024));
        assert_eq!(caps.nfs_mounts, vec![NfsMount { server: "10.0.10.61".into(), export: "/mnt/ssd9/models".into(), mountpoint: "/mnt/t440/ssd9-models".into() }]);
    }
}
