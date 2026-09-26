//! M10 — remote GPU job over a forced overlay path (Tailscale).
//!
//! `wgpu-remote` is not a second scheduler. A controller on one node sends a
//! typed compute job; the GPU node runs it in wgpu and returns the buffer.
//! `--via tailscale` binds and connects only on `100.64.0.0/10` so the proof
//! cannot silently fall back to LAN.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::{
    AdapterReport, Attachment, FabricError, Lease, Resource, ResourceAdapter, ResourceKind,
};

pub const ADAPTER_NAME: &str = "gpu-wgpu-remote";
pub const SCOPE: &str = "gpu-wgpu-remote-tailscale";
pub const DEFAULT_PORT: u16 = 9110;
/// Workload size for the live correctness bar.
pub const WORKLOAD_N: usize = 4096;

/// CGNAT used by Tailscale. LAN `192.168.4.0/22` is not in this range.
pub fn is_tailscale_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 100 && o[1] >= 64 && o[1] <= 127
        }
        IpAddr::V6(_) => false,
    }
}

pub fn require_tailscale(addr: SocketAddr, role: &str) -> Result<(), String> {
    if is_tailscale_ip(addr.ip()) {
        Ok(())
    } else {
        Err(format!(
            "{role} {addr} is not a Tailscale CGNAT address (100.64.0.0/10); \
             off-routing proof refuses LAN fallback"
        ))
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WgpuJob {
    pub job_id: String,
    pub op: String,
    pub n: usize,
    pub a: Vec<f32>,
    pub b: Vec<f32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WgpuResult {
    pub job_id: String,
    pub status: String,
    pub backend: String,
    pub adapter_name: String,
    pub out: Vec<f32>,
    pub elapsed_ms: u128,
    pub server_local: String,
    pub client_peer: String,
}

pub fn cpu_add(a: &[f32], b: &[f32]) -> Vec<f32> {
    a.iter().zip(b.iter()).map(|(x, y)| x + y).collect()
}

pub fn sample_job(n: usize) -> WgpuJob {
    let a: Vec<f32> = (0..n).map(|i| i as f32).collect();
    let b: Vec<f32> = (0..n).map(|i| i as f32 * 0.5).collect();
    WgpuJob {
        job_id: "m10-add".into(),
        op: "add_f32".into(),
        n,
        a,
        b,
    }
}

pub fn correctness_ok(job: &WgpuJob, out: &[f32]) -> bool {
    if out.len() != job.a.len() || out.len() != job.b.len() {
        return false;
    }
    cpu_add(&job.a, &job.b)
        .iter()
        .zip(out.iter())
        .all(|(expect, got)| (expect - got).abs() <= 1e-4)
}

fn write_frame(stream: &mut TcpStream, payload: &[u8]) -> Result<(), String> {
    let len = u32::try_from(payload.len()).map_err(|e| e.to_string())?;
    stream
        .write_all(&len.to_le_bytes())
        .map_err(|e| e.to_string())?;
    stream.write_all(payload).map_err(|e| e.to_string())
}

fn read_frame(stream: &mut TcpStream) -> Result<Vec<u8>, String> {
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).map_err(|e| e.to_string())?;
    let len = u32::from_le_bytes(header) as usize;
    if len > 16 * 1024 * 1024 {
        return Err("frame too large".into());
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

/// Run the add kernel. Uses wgpu when the feature is on; otherwise the
/// typed backend name is `cpu-reference` (tests / no-GPU hosts).
pub fn execute_add(job: &WgpuJob) -> Result<(Vec<f32>, String, String), String> {
    if job.op != "add_f32" {
        return Err(format!("unknown op {}", job.op));
    }
    if job.a.len() != job.n || job.b.len() != job.n {
        return Err("job length mismatch".into());
    }
    #[cfg(feature = "wgpu-remote")]
    {
        return crate::gpu::wgpu_kernel::add_f32(&job.a, &job.b);
    }
    #[cfg(not(feature = "wgpu-remote"))]
    {
        Ok((cpu_add(&job.a, &job.b), "cpu-reference".into(), "none".into()))
    }
}

pub fn serve(bind: SocketAddr) -> Result<(), String> {
    require_tailscale(bind, "bind")?;
    let listener = TcpListener::bind(bind).map_err(|e| format!("bind {bind}: {e}"))?;
    eprintln!("wgpu-remote listening on {bind} (Tailscale only)");
    for incoming in listener.incoming() {
        let mut stream = incoming.map_err(|e| e.to_string())?;
        let peer = stream.peer_addr().map_err(|e| e.to_string())?;
        if let Err(error) = require_tailscale(peer, "peer") {
            eprintln!("{error}");
            continue;
        }
        let local = stream.local_addr().map_err(|e| e.to_string())?;
        let started = Instant::now();
        let bytes = read_frame(&mut stream)?;
        let job: WgpuJob = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let (out, backend, adapter_name) = match execute_add(&job) {
            Ok(tuple) => tuple,
            Err(error) => {
                let fail = WgpuResult {
                    job_id: job.job_id,
                    status: "error".into(),
                    backend: "error".into(),
                    adapter_name: error,
                    out: vec![],
                    elapsed_ms: started.elapsed().as_millis(),
                    server_local: local.to_string(),
                    client_peer: peer.to_string(),
                };
                write_frame(&mut stream, &serde_json::to_vec(&fail).map_err(|e| e.to_string())?)?;
                continue;
            }
        };
        let status = if correctness_ok(&job, &out) {
            "ok".into()
        } else {
            "incorrect".into()
        };
        let result = WgpuResult {
            job_id: job.job_id,
            status,
            backend,
            adapter_name,
            out,
            elapsed_ms: started.elapsed().as_millis(),
            server_local: local.to_string(),
            client_peer: peer.to_string(),
        };
        write_frame(&mut stream, &serde_json::to_vec(&result).map_err(|e| e.to_string())?)?;
    }
    Ok(())
}

pub fn submit(connect: SocketAddr, job: &WgpuJob) -> Result<WgpuResult, String> {
    require_tailscale(connect, "connect")?;
    let mut stream = TcpStream::connect_timeout(&connect, Duration::from_secs(8))
        .map_err(|e| format!("connect {connect}: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .ok();
    let local = stream.local_addr().map_err(|e| e.to_string())?;
    require_tailscale(local, "client-local")?;
    write_frame(&mut stream, &serde_json::to_vec(job).map_err(|e| e.to_string())?)?;
    let bytes = read_frame(&mut stream)?;
    let mut result: WgpuResult = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if result.status == "ok" && !correctness_ok(job, &result.out) {
        result.status = "incorrect".into();
    }
    Ok(result)
}

pub fn default_resource(node: impl Into<String>) -> Resource {
    Resource {
        id: "local.gpu.wgpu-remote".into(),
        kind: ResourceKind::Gpu,
        capacity: 1,
        unit: "remote-job".into(),
        node: node.into(),
        state: crate::ResourceState::Available,
        exclusive: true,
        attributes: std::collections::BTreeMap::from([
            ("gpu.transport".into(), "tailscale".into()),
            ("gpu.protocol".into(), "wgpu-remote/1".into()),
            ("gpu.port".into(), DEFAULT_PORT.to_string()),
        ]),
    }
}

#[derive(Debug, Default)]
pub struct WgpuRemoteAdapter;

impl ResourceAdapter for WgpuRemoteAdapter {
    fn name(&self) -> &str {
        ADAPTER_NAME
    }

    fn attach(&self, resource: &Resource, lease: &Lease) -> Result<Attachment, FabricError> {
        if resource.kind != ResourceKind::Gpu {
            return Err(FabricError::IncompatibleResourceKind {
                adapter: ADAPTER_NAME.into(),
                expected: ResourceKind::Gpu,
                actual: resource.kind,
            });
        }
        Ok(Attachment {
            resource_id: resource.id.clone(),
            lease_id: lease.id,
            adapter: ADAPTER_NAME.into(),
            details: std::collections::BTreeMap::from([
                ("gpu.transport".into(), "tailscale".into()),
                ("gpu.protocol".into(), "wgpu-remote/1".into()),
            ]),
        })
    }

    fn capability_report(&self) -> AdapterReport {
        AdapterReport {
            name: ADAPTER_NAME.into(),
            scope: SCOPE.into(),
            healthy: cfg!(feature = "wgpu-remote"),
            detail: std::collections::BTreeMap::from([
                ("transport".into(), "tailscale-cgnat".into()),
                (
                    "wgpu".into(),
                    if cfg!(feature = "wgpu-remote") {
                        "enabled"
                    } else {
                        "feature-off"
                    }
                    .into(),
                ),
            ]),
        }
    }
}

/// Helper for tests / scripts: parse `100.x.y.z:port`.
pub fn parse_bind(raw: &str) -> Result<SocketAddr, String> {
    raw.parse::<SocketAddr>()
        .map_err(|e| format!("not a socket addr: {e}"))
}

pub fn tailscale_v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(a, b, c, d))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tailscale_cgnat_is_accepted_lan_is_not() {
        assert!(is_tailscale_ip(tailscale_v4(100, 109, 55, 22)));
        assert!(is_tailscale_ip(tailscale_v4(100, 76, 255, 40)));
        assert!(!is_tailscale_ip(IpAddr::V4(Ipv4Addr::new(192, 168, 4, 38))));
        assert!(!is_tailscale_ip(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
    }

    #[test]
    fn cpu_reference_meets_correctness_bar() {
        let job = sample_job(64);
        let out = cpu_add(&job.a, &job.b);
        assert!(correctness_ok(&job, &out));
        assert!(!correctness_ok(&job, &vec![0.0; 64]));
    }

    #[test]
    fn require_tailscale_rejects_lan_bind() {
        let lan: SocketAddr = "192.168.4.38:9110".parse().unwrap();
        assert!(require_tailscale(lan, "bind").is_err());
        let ts: SocketAddr = "100.109.55.22:9110".parse().unwrap();
        assert!(require_tailscale(ts, "bind").is_ok());
    }
}
