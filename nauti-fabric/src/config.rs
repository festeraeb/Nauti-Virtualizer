//! Agent configuration loading from TOML files.
//!
//! The agent reads its runtime configuration from a TOML file (default
//! `/etc/nauti/agent.toml`, override with `NAUTI_AGENT_CONFIG` env var).
//! This is separate from the crate's Cargo features; features control *what
//! code is compiled*, while the TOML config controls *how the compiled
//! features behave at runtime*.
//!
//! The schema is documented in `scripts/nauti-agent.toml.sample` and the
//! generated systemd unit is in `scripts/nauti-agent.service.sample`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Top-level agent configuration.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct AgentConfig {
    /// Node identifier for this agent (used in Fabric resource node field).
    #[serde(default = "default_node")]
    pub node: String,
    /// Runtime fabric settings.
    #[serde(default)]
    pub fabric: FabricConfig,
    /// Authentication provider settings.
    #[serde(default)]
    pub auth: AuthConfig,
    /// VFIO passthrough settings.
    #[cfg(feature = "vfio")]
    #[serde(default)]
    pub vfio: VfioConfig,
    /// Cloud Hypervisor adapter settings.
    #[cfg(feature = "cloud-hypervisor")]
    #[serde(default)]
    pub cloud_hypervisor: CloudHypervisorConfig,
}

fn default_node() -> String {
    "local".to_string()
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct FabricConfig {
    /// How often to auto-refresh the local inventory (seconds). 0 = disabled.
    #[serde(default)]
    pub auto_refresh_interval: u64,
}

/// Authentication provider configuration. Exactly one method is active.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct AuthConfig {
    /// One of: "none" (default), "ttl", "oauth2", "shared-secret".
    #[serde(default = "default_auth_method")]
    pub method: String,
    /// TTL bounds for the "ttl" method.
    #[serde(default)]
    pub ttl: TtlConfig,
    /// OAuth2 settings for the "oauth2" method.
    #[serde(default)]
    pub oauth2: OAuth2Config,
    /// Shared secret settings for the "shared-secret" method.
    #[serde(default)]
    pub shared_secret: SharedSecretConfig,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            method: default_auth_method(),
            ttl: TtlConfig::default(),
            oauth2: OAuth2Config::default(),
            shared_secret: SharedSecretConfig::default(),
        }
    }
}

fn default_auth_method() -> String {
    "none".to_string()
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct TtlConfig {
    /// Minimum TTL in seconds (rejects LeaseExclusive below this).
    #[serde(default = "default_ttl_min")]
    pub min_secs: u64,
    /// Maximum TTL in seconds (rejects LeaseExclusive above this).
    #[serde(default = "default_ttl_max")]
    pub max_secs: u64,
}

fn default_ttl_min() -> u64 { 1 }
fn default_ttl_max() -> u64 { 3600 }

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct OAuth2Config {
    /// URL to fetch the JWKS (JSON Web Key Set) from.
    pub jwks_url: Option<String>,
    /// Expected audience ("aud" claim).
    pub audience: Option<String>,
    /// Path to a file containing the client ID/secret if needed.
    pub client_secrets_file: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct SharedSecretConfig {
    /// Path to a file containing the shared secret (mode 0600).
    pub file: Option<PathBuf>,
}

#[cfg(feature = "vfio")]
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct VfioConfig {
    /// PCI vendor IDs to exclude from VFIO binding (comma-separated hex).
    /// Example: "10de,1002" to skip NVIDIA and AMD GPUs.
    pub exclude_vendors: Option<String>,
    /// IOMMU group IDs to exclude from passthrough.
    pub exclude_iommu_groups: Option<String>,
    /// If true, automatically attempt to bind unclaimed GPUs to vfio-pci
    /// at startup (requires root and driverctl). Default false.
    #[serde(default)]
    pub auto_bind: bool,
}

#[cfg(feature = "cloud-hypervisor")]
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct CloudHypervisorConfig {
    /// Default path to the cloud-hypervisor binary.
    #[serde(default = "default_ch_binary")]
    pub binary: PathBuf,
    /// Default API socket directory.
    #[serde(default = "default_ch_socket_dir")]
    pub socket_dir: PathBuf,
    /// Default kernel path.
    pub kernel: Option<PathBuf>,
    /// Default rootfs path.
    pub rootfs: Option<PathBuf>,
    /// Default vCPUs per VM.
    #[serde(default = "default_vcpus")]
    pub vcpus: u32,
    /// Default memory in MiB per VM.
    #[serde(default = "default_memory_mib")]
    pub memory_mib: u32,
}

#[allow(dead_code)] // reserved: wired into vm launch default resolution (follow-up)
fn default_ch_binary() -> PathBuf {
    PathBuf::from("/usr/bin/cloud-hypervisor")
}
#[allow(dead_code)] // serde default fn; referenced by attribute string, invisible to dead-code analysis
fn default_ch_socket_dir() -> PathBuf {
    PathBuf::from("/var/run/nauti")
}
#[allow(dead_code)] // serde default fn; referenced by attribute string, invisible to dead-code analysis
fn default_vcpus() -> u32 { 1 }
#[allow(dead_code)] // serde default fn; referenced by attribute string, invisible to dead-code analysis
fn default_memory_mib() -> u32 { 512 }

/// Errors from config loading.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config file not found: {0}")]
    NotFound(PathBuf),
    #[error("config file could not be read: {0}")]
    Unreadable(PathBuf, std::io::Error),
    #[error("config file has invalid TOML syntax: {0}")]
    InvalidToml(toml::de::Error),
    #[error("config file has missing/invalid value: {0}")]
    InvalidValue(String),
    #[error("auth method '{0}' requires feature that is not enabled")]
    FeatureNotEnabled(String),
}

impl AgentConfig {
    /// Load configuration from the default path (`/etc/nauti/agent.toml`) or
    /// the path specified in `NAUTI_AGENT_CONFIG` env var. If neither exists,
    /// returns a default config (NoAuth, no VFIO, etc.).
    pub fn load() -> Result<Self, ConfigError> {
        let path = std::env::var("NAUTI_AGENT_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/etc/nauti/agent.toml"));
        Self::from_file(&path)
    }

    /// Load from an explicit file path.
    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            // No config file is OK; return defaults.
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::Unreadable(path.to_path_buf(), e))?;
        toml::from_str(&text).map_err(ConfigError::InvalidToml)
    }

    /// Validate the config and return a list of warnings (non-fatal).
    /// The config is still usable if this returns non-empty warnings.
    pub fn validate(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        
        if self.auth.method == "ttl" && !cfg!(feature = "auth-ttl") {
            warnings.push("auth method 'ttl' selected but --features auth-ttl is not enabled; falling back to 'none'".to_string());
        }
        if self.auth.method == "oauth2" && !cfg!(feature = "auth-oauth") {
            warnings.push("auth method 'oauth2' selected but --features auth-oauth is not enabled; falling back to 'none'".to_string());
        }
        if self.auth.method == "shared-secret" && !cfg!(feature = "auth-shared-secret") {
            warnings.push("auth method 'shared-secret' selected but --features auth-shared-secret is not enabled; falling back to 'none'".to_string());
        }
        if self.fabric.auto_refresh_interval > 0 { 
            // Valid but note it's a feature not yet implemented in the agent loop.
            warnings.push("fabric.auto_refresh_interval is set but not yet implemented in agent loop".to_string());
        }
        
        #[cfg(feature = "vfio")]
        {
            if self.vfio.auto_bind { 
                warnings.push("vfio.auto_bind is experimental; requires root and driverctl installed".to_string());
            }
        }
        
        #[cfg(feature = "cloud-hypervisor")]
        {
            if self.cloud_hypervisor.kernel.is_none() || self.cloud_hypervisor.rootfs.is_none() {
                warnings.push("cloud_hypervisor.kernel and rootfs should be set for VM launch to work".to_string());
            }
        }

        warnings
    }

    /// Build the concrete [`AuthProvider`] from the config. Returns `NoAuth`
    /// if the selected method's feature is not enabled (with a logged warning).
    pub fn build_auth_provider(&self) -> Box<dyn crate::rpc_auth::AuthProvider> {
        match self.auth.method.as_str() {
            "ttl" => {
                #[cfg(feature = "auth-ttl")] {
                    return Box::new(crate::rpc_auth::TtlBound {
                        min: std::time::Duration::from_secs(self.auth.ttl.min_secs),
                        max: std::time::Duration::from_secs(self.auth.ttl.max_secs),
                    });
                }
                #[cfg(not(feature = "auth-ttl"))] {
                    eprintln!("WARN: auth method 'ttl' selected but --features auth-ttl not enabled; using NoAuth");
                    return Box::new(crate::rpc_auth::NoAuth);
                }
            }
            "oauth2" => {
                #[cfg(feature = "auth-oauth")] {
                    return Box::new(crate::rpc_auth::OAuth2Bearer {
                        jwks_url: self.auth.oauth2.jwks_url.clone(),
                        audience: self.auth.oauth2.audience.clone(),
                    });
                }
                #[cfg(not(feature = "auth-oauth"))] {
                    eprintln!("WARN: auth method 'oauth2' selected but --features auth-oauth not enabled; using NoAuth");
                    return Box::new(crate::rpc_auth::NoAuth);
                }
            }
            "shared-secret" => {
                #[cfg(feature = "auth-shared-secret")] {
                    return Box::new(crate::rpc_auth::SharedSecret {
                        token: self.auth.shared_secret.file.as_ref().map(|p| {
                            std::fs::read_to_string(p).unwrap_or_default().trim().to_string()
                        }),
                    });
                }
                #[cfg(not(feature = "auth-shared-secret"))] {
                    eprintln!("WARN: auth method 'shared-secret' selected but --features auth-shared-secret not enabled; using NoAuth");
                    return Box::new(crate::rpc_auth::NoAuth);
                }
            }
            _ => { /* default: NoAuth */ }
        }
        Box::new(crate::rpc_auth::NoAuth)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn load_missing_file_returns_default() {
        let config = AgentConfig::from_file(Path::new("/nonexistent/path.toml")).unwrap();
        assert_eq!(config.auth.method, "none");
    }

    #[test]
    fn load_valid_toml() {
        let mut file = NamedTempFile::new().unwrap();
        std::io::Write::write_all(file.as_file_mut(), b"node = \"test-node\"
fabric.auto-refresh-interval = 300

auth.method = \"none\"
").unwrap();
        let config = AgentConfig::from_file(file.path()).unwrap();
        assert_eq!(config.node, "test-node");
        assert_eq!(config.fabric.auto_refresh_interval, 300);
        assert_eq!(config.auth.method, "none");
    }

    #[test]
    fn invalid_toml_returns_error() {
        let mut file = NamedTempFile::new().unwrap();
        std::io::Write::write_all(file.as_file_mut(), b"invalid toml [[[").unwrap();
        let err = AgentConfig::from_file(file.path()).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidToml(_)));
    }

    #[test]
    fn build_auth_provider_noauth_always_works() {
        let config = AgentConfig::default();
        let provider = config.build_auth_provider();
        assert_eq!(provider.name(), "none");
        let ctx = crate::rpc_auth::RequestContext {
            remote_endpoint_id: "test".into(),
            local_endpoint_id: "local".into(),
        };
        let req = crate::rpc::RpcRequest::Ping;
        provider.authorize(&ctx, &req).unwrap();
    }
}
