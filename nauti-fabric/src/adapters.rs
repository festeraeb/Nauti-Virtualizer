//! Adapters for resources attached on the current host or reached over the fabric network.

use std::collections::BTreeMap;

use crate::{Attachment, FabricError, Lease, Resource, ResourceAdapter, ResourceKind};

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ResourceState;

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
}