//! Adapters for resources attached on the current host or reached over the fabric network.

use std::collections::BTreeMap;

use crate::{AdapterReport, Attachment, FabricError, Lease, Resource, ResourceAdapter};

fn attachment(resource: &Resource, lease: &Lease, adapter: &str, details: BTreeMap<String, String>) -> Attachment {
    Attachment {
        resource_id: resource.id.clone(),
        lease_id: lease.id,
        adapter: adapter.into(),
        details,
    }
}

/// Describes a current-host attachment without mounting, binding, or mutating hardware.
#[derive(Debug, Default)]
pub struct LocalResourceAdapter;

impl ResourceAdapter for LocalResourceAdapter {
    fn name(&self) -> &str {
        "local-resource"
    }

    fn attach(&self, resource: &Resource, lease: &Lease) -> Result<Attachment, FabricError> {
        if resource.node != "local" {
            return Err(FabricError::IncompatibleResourceLocality {
                adapter: self.name().into(),
                expected: "local".into(),
                actual: resource.node.clone(),
            });
        }
        Ok(attachment(
            resource,
            lease,
            self.name(),
            BTreeMap::from([("attachment.scope".into(), "local".into())]),
        ))
    }

    fn capability_report(&self) -> AdapterReport {
        AdapterReport {
            name: self.name().into(),
            scope: "local-host".into(),
            healthy: true,
            detail: BTreeMap::from([("resource.locality".into(), "local".into())]),
        }
    }
}

/// Produces a remote attachment descriptor for a resource advertised by a fabric agent.
///
/// The adapter intentionally does not select or open a transport. A remote agent must
/// publish a `network.endpoint` and `network.protocol`, and a later transport adapter
/// must authenticate that endpoint before resource use.
#[derive(Debug, Default)]
pub struct NetworkResourceAdapter;

impl ResourceAdapter for NetworkResourceAdapter {
    fn name(&self) -> &str {
        "network-resource"
    }

    fn attach(&self, resource: &Resource, lease: &Lease) -> Result<Attachment, FabricError> {
        if resource.node == "local" {
            return Err(FabricError::IncompatibleResourceLocality {
                adapter: self.name().into(),
                expected: "remote".into(),
                actual: "local".into(),
            });
        }
        let endpoint = resource
            .attributes
            .get("network.endpoint")
            .cloned()
            .ok_or_else(|| FabricError::MissingResourceAttribute("network.endpoint".into()))?;
        let protocol = resource
            .attributes
            .get("network.protocol")
            .cloned()
            .ok_or_else(|| FabricError::MissingResourceAttribute("network.protocol".into()))?;
        Ok(attachment(
            resource,
            lease,
            self.name(),
            BTreeMap::from([
                ("attachment.scope".into(), "network".into()),
                ("endpoint".into(), endpoint),
                ("protocol".into(), protocol),
            ]),
        ))
    }

    fn capability_report(&self) -> AdapterReport {
        AdapterReport {
            name: self.name().into(),
            scope: "remote-descriptor".into(),
            healthy: true,
            detail: BTreeMap::from([("resource.locality".into(), "remote".into())]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ResourceKind, ResourceState};

    fn resource(node: &str, attributes: BTreeMap<String, String>) -> Resource {
        Resource {
            id: "gpu.0".into(),
            kind: ResourceKind::Gpu,
            capacity: 1,
            unit: "device".into(),
            node: node.into(),
            state: ResourceState::Available,
            exclusive: true,
            attributes,
        }
    }

    fn lease() -> Lease {
        Lease { id: 1, resource_id: "gpu.0".into(), owner: "test".into() }
    }

    #[test]
    fn local_adapter_only_accepts_local_resources() {
        assert!(LocalResourceAdapter.attach(&resource("local", BTreeMap::new()), &lease()).is_ok());
        assert!(LocalResourceAdapter.attach(&resource("node-b", BTreeMap::new()), &lease()).is_err());
    }

    #[test]
    fn network_adapter_requires_a_remote_endpoint_and_protocol() {
        let remote = resource(
            "node-b",
            BTreeMap::from([
                ("network.endpoint".into(), "node-b.example:4433".into()),
                ("network.protocol".into(), "quic".into()),
            ]),
        );
        let attachment = NetworkResourceAdapter.attach(&remote, &lease()).unwrap();
        assert_eq!(attachment.details["attachment.scope"], "network");
        assert_eq!(attachment.details["protocol"], "quic");
    }

    #[test]
    fn local_adapter_reports_healthy_local_scope() {
        let report = LocalResourceAdapter.capability_report();
        assert_eq!(report.name, "local-resource");
        assert_eq!(report.scope, "local-host");
        assert!(report.healthy);
    }

    #[test]
    fn network_adapter_reports_healthy_remote_scope() {
        let report = NetworkResourceAdapter.capability_report();
        assert_eq!(report.name, "network-resource");
        assert_eq!(report.scope, "remote-descriptor");
        assert!(report.healthy);
    }
}
// ---------------------------------------------------------------------------
// Lemonade adapter
// ---------------------------------------------------------------------------

/// Config for reaching a Lemonade server. Lemonade hosts and manages LLM
/// inference (any GPU brand via its Vulkan/CUDA/ROCm backends) and exposes a
/// CLI that reports status and loaded models. This adapter treats Lemonade as
/// the owning backend: it asks Lemonade what it is serving rather than driving
/// the GPU directly.
///
/// This is the "bring Lemonade into the VM and let it report what it's
/// serving" pattern: the fabric never touches ROCm or Vulkan itself, it just
/// mirrors Lemonade's capability report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LemonadeConfig {
    /// Lemonade server host. Default 127.0.0.1.
    pub host: String,
    /// Lemonade server port. Default 13305.
    pub port: u16,
    /// Optional API key for authenticated endpoints.
    pub api_key: Option<String>,
}

impl Default for LemonadeConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 13305,
            api_key: None,
        }
    }
}

/// What `lemonade status` + `lemonade list --downloaded` told us, parsed for
/// the capability report. Field presence reflects Lemonade's answer; nothing
/// is invented.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LemonadeReport {
    /// True if Lemonade reported a running server (it answered /v1/models or
    /// `status` successfully).
    pub reachable: bool,
    /// Lemonade's advertised version (e.g. "11.9.0"), if reported.
    pub version: Option<String>,
    /// Models Lemonade reports as downloaded/loadable, one per backend.
    pub models: Vec<LemonadeModel>,
}

/// A model Lemonade can serve, tied to a backend (the compute path Lemonade
/// would use for it — e.g. `vulkan`, `cuda`, `rocm`, `cpu`).
#[derive(Clone, Debug, PartialEq)]
pub struct LemonadeModel {
    pub id: String,
    pub backend: String,
    pub downloaded: bool,
    pub size_gb: Option<f64>,
}

/// A `ResourceAdapter` delegate that surfaces a Lemonade server's capability.
///
/// `attach` only records the lease and reports the Lemonade endpoint; it does
/// not open a transport or load a model (Lemonade owns model lifecycle). The
/// capability report is the live Lemonade status.
#[derive(Clone, Debug)]
pub struct LemonadeAdapter {
    config: LemonadeConfig,
}

impl LemonadeAdapter {
    pub fn new(config: LemonadeConfig) -> Self {
        Self { config }
    }

    /// Query the Lemonade server and produce a structured report. Pure CLI
    /// inspection:
    ///   - `lemonade --host H --port P status`
    ///   - `lemonade --host H --port P list --downloaded`
    /// Missing/unsuccessful CLI calls are folded into `reachable: false`
    /// rather than panicking.
    pub fn status(&self) -> LemonadeReport {
        let mut report = LemonadeReport::default();
        let base = format!("{}:{}", self.config.host, self.config.port);

        // `lemonade status` — server liveness + version.
        if let Some(out) = run_lemonade("status", &base, self.config.api_key.as_deref()) {
            report.reachable = out.to_lowercase().contains("running")
                || out.to_lowercase().contains("server is running");
            // Version line looks like:  Version   11.9.0
            report.version = out
                .lines()
                .find(|l| l.trim().starts_with("Version"))
                .and_then(|l| l.split_whitespace().nth(1))
                .map(str::to_string);
        }

        // `lemonade list --downloaded` — available models. Columns are:
        //   Model Name   Downloaded   Size (GB)   Details (backend)
        // header row, then a dashed separator, then one row per model.
        if let Some(out) = run_lemonade("list --downloaded", &base, self.config.api_key.as_deref()) {
            for line in out.lines().skip(2) {
                // Skip the trailing dashed separator row.
                if line.trim().is_empty() || line.trim().starts_with("---") {
                    continue;
                }
                let mut parts = line.split_whitespace();
                let id = parts.next().unwrap_or("").trim().to_string();
                let downloaded = parts.next().unwrap_or("no").eq_ignore_ascii_case("yes");
                let size = parts.next().and_then(|s| s.parse::<f64>().ok());
                // Backend is the 4th column, e.g. "llamacpp".
                let backend = parts.next().unwrap_or("unknown").trim().to_string();
                if !id.is_empty() && id != "Model" {
                    report.models.push(LemonadeModel {
                        id,
                        backend,
                        downloaded,
                        size_gb: size,
                    });
                }
            }
        }
        report
    }
}

fn run_lemonade(args: &str, base: &str, api_key: Option<&str>) -> Option<String> {
    use std::process::Command;
    let mut cmd = Command::new("lemonade");
    cmd.arg("--host").arg(base.split(':').next().unwrap_or("127.0.0.1"));
    cmd.arg("--port").arg(base.split(':').nth(1).unwrap_or("13305"));
    if let Some(key) = api_key {
        cmd.arg("--api-key").arg(key);
    }
    // Split args safely: the subcommands are space-separated words.
    let parts: Vec<&str> = args.split(' ').collect();
    cmd.args(&parts);
    cmd.output().ok().and_then(|o| {
        if o.status.success() {
            Some(String::from_utf8_lossy(&o.stdout).into_owned())
        } else {
            // `status`/`list` return non-zero if the server is unreachable;
            // that is folded to None (reachable=false).
            None
        }
    })
}

impl ResourceAdapter for LemonadeAdapter {
    fn name(&self) -> &str {
        "lemonade"
    }

    fn attach(&self, resource: &Resource, lease: &Lease) -> Result<Attachment, FabricError> {
        // Lemonade is a service backend, not a GPU/VM device. Accept any
        // resource carrying a lemonade.endpoint attribute so callers can
        // reserve the serving slot.
        let endpoint = resource
            .attributes
            .get("lemonade.endpoint")
            .cloned()
            .or_else(|| {
                Some(format!("{}:{}", self.config.host, self.config.port))
            });
        let report = self.status();
        let mut details = BTreeMap::from([
            ("lemonade.endpoint".into(), endpoint.unwrap_or_default()),
            ("lemonade.reachable".into(), report.reachable.to_string()),
            ("attachment.scope".into(), "lemonade-serving".into()),
        ]);
        if let Some(version) = &report.version {
            details.insert("lemonade.version".into(), version.clone());
        }
        if !report.models.is_empty() {
            let ids: Vec<String> = report.models.iter().map(|m| m.id.clone()).collect();
            details.insert("lemonade.models".into(), ids.join(","));
        }
        if let Some(b) = report.models.iter().map(|m| m.backend.as_str()).find(|b| *b != "") {
            details.insert("lemonade.backend".into(), b.to_string());
        }
        Ok(attachment(resource, lease, self.name(), details))
    }

    fn capability_report(&self) -> AdapterReport {
        let report = self.status();
        let mut detail = BTreeMap::from([
            ("lemonade.endpoint".into(), format!("{}:{}", self.config.host, self.config.port)),
            (
                "lemonade.reachable".into(),
                report.reachable.to_string(),
            ),
        ]);
        if let Some(version) = &report.version {
            detail.insert("lemonade.version".into(), version.clone());
        }
        if report.models.is_empty() {
            detail.insert("lemonade.models".into(), "none-loaded".into());
        } else {
            let ids: Vec<String> = report.models.iter().map(|m| m.id.clone()).collect();
            detail.insert("lemonade.models".into(), ids.join(","));
            let backends: Vec<&str> = report
                .models
                .iter()
                .map(|m| m.backend.as_str())
                .filter(|b| !b.is_empty())
                .collect();
            if !backends.is_empty() {
                detail.insert("lemonade.backend".into(), backends.join(","));
            }
        }
        AdapterReport {
            name: self.name().into(),
            scope: "lemonade-serving".into(),
            healthy: report.reachable,
            detail,
        }
    }
}

#[cfg(test)]
mod lemonade_tests {
    use super::*;

    #[test]
    fn default_config_points_at_localhost_13305() {
        let cfg = LemonadeConfig::default();
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 13305);
    }

    #[test]
    fn report_untouched_by_fixture() {
        // A reachable Lemonade report carries the fields we care about.
        let report = LemonadeReport {
            reachable: true,
            version: Some("11.9.0".into()),
            models: vec![LemonadeModel {
                id: "Qwen3-0.6B-GGUF".into(),
                backend: "llamacpp".into(),
                downloaded: true,
                size_gb: Some(0.36),
            }],
        };
        assert!(report.reachable);
        assert_eq!(report.version.as_deref(), Some("11.9.0"));
        assert_eq!(report.models.len(), 1);
    }

    #[test]
    fn attach_does_not_invent_health() {
        // Build an adapter pointed at a likely-unreachable port so the report
        // is honest (reachable=false) rather than fabricated.
        let adapter = LemonadeAdapter::new(LemonadeConfig {
            host: "127.0.0.1".into(),
            port: 1, // nothing listens here
            api_key: None,
        });
        let resource = Resource {
            id: "lemonade.0".into(),
            kind: crate::ResourceKind::Device,
            capacity: 1,
            unit: "slot".into(),
            node: "local".into(),
            state: crate::ResourceState::Available,
            exclusive: true,
            attributes: BTreeMap::new(),
        };
        let lease = Lease { id: 1, resource_id: "lemonade.0".into(), owner: "test".into() };
        let attachment = adapter.attach(&resource, &lease).unwrap();
        // reachable should track reality (unreachable port), not claim true.
        assert_eq!(attachment.details["lemonade.reachable"], "false");
    }
}
