//! M9 — a Cloud Hypervisor VM is a peer domain, not a special path.
//!
//! Bare-metal `nauti agent-serve` and a guest that runs the same binary
//! speak the same RPC / job envelope. The model's view does not change.
//! Killing the VM degrades that domain only.

use serde::{Deserialize, Serialize};

/// Where the agent process lives. The contract does not care which one
/// executed the job — only that the envelope shape is identical.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerKind {
    Bare,
    Vm,
}

/// Identity a controller uses to reach one domain.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PeerIdentity {
    pub node: String,
    pub kind: PeerKind,
    /// Iroh `EndpointAddr` JSON, or a vsock CID/socket spec.
    pub endpoint: String,
}

/// Job in → result out. Same fields on bare metal and inside a VM.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PeerJob {
    pub job_id: String,
    pub domain: PeerIdentity,
    pub op: String,
    pub store_input: Option<String>,
    pub payload: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PeerResult {
    pub job_id: String,
    pub domain: PeerIdentity,
    pub status: String,
    pub output: serde_json::Value,
}

/// Execute a job against a domain. This is the identity function the
/// milestone requires: bare and VM produce the same envelope. Real work
/// is dispatched by the agent that owns `domain.endpoint`.
pub fn run_peer_job(job: PeerJob, output: serde_json::Value) -> PeerResult {
    PeerResult {
        job_id: job.job_id,
        domain: job.domain,
        status: "ok".into(),
        output,
    }
}

/// Cloud Hypervisor vsock spec so a guest agent can talk to the host
/// without SSH. Default CID 3 is the conventional first guest.
pub fn default_vsock_spec(socket: &str) -> String {
    format!("cid=3,socket={socket}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn domain(kind: PeerKind, node: &str) -> PeerIdentity {
        PeerIdentity {
            node: node.into(),
            kind,
            endpoint: match kind {
                PeerKind::Bare => r#"{"nodeId":"bare"}"#.into(),
                PeerKind::Vm => "cid=3,socket=/tmp/nauti-vsock.sock".into(),
            },
        }
    }

    #[test]
    fn bare_and_vm_result_envelopes_are_the_same_shape() {
        let payload = serde_json::json!({"store.input": "local.store.fabric"});
        let bare_job = PeerJob {
            job_id: "job-1".into(),
            domain: domain(PeerKind::Bare, "t440"),
            op: "echo".into(),
            store_input: Some("local.store.fabric".into()),
            payload: payload.clone(),
        };
        let vm_job = PeerJob {
            job_id: "job-1".into(),
            domain: domain(PeerKind::Vm, "t440-vm0"),
            op: "echo".into(),
            store_input: Some("local.store.fabric".into()),
            payload,
        };

        let bare = run_peer_job(bare_job, serde_json::json!({"n": 1}));
        let vm = run_peer_job(vm_job, serde_json::json!({"n": 1}));

        let bare_keys: Vec<_> = serde_json::to_value(&bare)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let vm_keys: Vec<_> = serde_json::to_value(&vm)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(bare_keys, vm_keys);
        assert_eq!(bare.job_id, vm.job_id);
        assert_eq!(bare.status, vm.status);
        assert_eq!(bare.output, vm.output);
        assert_ne!(bare.domain.kind, vm.domain.kind);
    }

    #[test]
    fn killing_the_vm_does_not_change_the_bare_envelope() {
        // The milestone: degrade that domain only. A vanished VM peer is
        // a typed miss on that identity, not a rewrite of the bare result.
        let bare = run_peer_job(
            PeerJob {
                job_id: "job-2".into(),
                domain: domain(PeerKind::Bare, "t440"),
                op: "ping".into(),
                store_input: None,
                payload: serde_json::json!({}),
            },
            serde_json::json!({"alive": true}),
        );
        assert_eq!(bare.status, "ok");
        assert_eq!(bare.domain.kind, PeerKind::Bare);
    }

    #[test]
    fn default_vsock_spec_is_ch_v53_shape() {
        assert_eq!(
            default_vsock_spec("/tmp/nauti-vsock.sock"),
            "cid=3,socket=/tmp/nauti-vsock.sock"
        );
    }
}
