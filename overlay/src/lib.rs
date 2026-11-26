//! ModelMistress's Transferd overlay node, shared by the desktop tool (`mm-overlay`) and the Android app.
//!
//! A node keeps its identity in `<home>/identity.seed` (64 random bytes, created once; never overwritten), its overlay
//! state in `<home>/overlay-state.json` and its audit trail in `<home>/overlay-audit.jsonl`. It dials out to relays and
//! to the direct addresses peers publish. It opens an inbound port only when `Settings::listen` is set.
//!
//! The desktop exposes the hub's loopback port as the service [`HUB_SERVICE`], for paired nodes only ([`ALLOW_PAIRED`]).
//! A phone that pairs with the desktop connects to that service and gets a loopback address; the app uses that address
//! as the desktop's hub address. The hub's own authentication (the peer key) is unchanged.

use std::io::Write;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use transferd_overlay::identity::{self, NodeIdentity};
use transferd_overlay::node::{OverlayConfig, OverlayNode};

/// The service name the desktop publishes its hub under.
pub const HUB_SERVICE: &str = "modelmistress-hub";
/// The access rule for a service: any node that paired through an invite, and no other.
pub const ALLOW_PAIRED: &str = "*paired";

const SEED_FILE: &str = "identity.seed";
const STATE_FILE: &str = "overlay-state.json";
const AUDIT_FILE: &str = "overlay-audit.jsonl";
/// How long `connect` may take to reach a peer before it is reported as unreachable.
const CONNECT_WAIT: Duration = Duration::from_secs(45);

/// How a node is set up. `relays` are `host:port` UDP relays it registers on; `direct` are addresses it publishes for
/// direct dialling (they travel in its invites); `listen` is an optional direct TCP listener (none by default).
#[derive(Clone, Debug, Default)]
pub struct Settings {
    pub name: String,
    pub relays: Vec<String>,
    pub direct: Vec<String>,
    pub listen: Option<SocketAddr>,
}

/// A node that was just paired with, and the reachability its invite carried.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paired {
    pub node_id: String,
    pub name: String,
    pub relays: Vec<String>,
    pub direct: Vec<String>,
}

/// A running overlay node. Cloning it gives another handle to the same node.
#[derive(Clone)]
pub struct Overlay {
    node: Arc<OverlayNode>,
}

impl Overlay {
    /// Start the node whose state lives in `home`. The first start creates the identity.
    pub async fn start(home: &Path, settings: Settings) -> Result<Self, String> {
        std::fs::create_dir_all(home)
            .map_err(|e| format!("cannot create the overlay folder {}: {e}", home.display()))?;
        let identity = load_identity(&home.join(SEED_FILE), &settings.name)?;
        let mut config = OverlayConfig::local(identity);
        config.relays = settings.relays;
        config.direct = settings.direct;
        config.listen = settings.listen;
        config.state_path = Some(home.join(STATE_FILE));
        config.audit_path = Some(home.join(AUDIT_FILE));
        let node = OverlayNode::start(config).await.map_err(|e| e.to_string())?;
        Ok(Self { node })
    }

    /// This node's id: 64 hex characters, the name other nodes pair with and connect to.
    pub fn node_id(&self) -> String {
        self.node.node_id()
    }

    /// The direct listener's address, when the node has one.
    pub fn listener(&self) -> Option<SocketAddr> {
        self.node.listener_addr()
    }

    /// The relays this node is registered on.
    pub fn relays(&self) -> Vec<String> {
        self.node.relays().iter().map(|a| a.to_string()).collect()
    }

    /// A pairing invite, valid for `ttl_s` seconds. The node must stay running until the invite is used.
    pub fn invite(&self, ttl_s: u64) -> String {
        self.node.invite_create(ttl_s)
    }

    /// Pair with the node that made `invite`. Returns its id and the relays and addresses the invite carried.
    pub fn accept(&self, invite: &str, name: Option<String>) -> Result<Paired, String> {
        let reach = identity::invite_reach(invite);
        let record = self.node.invite_accept(invite, name).map_err(|e| e.to_string())?;
        Ok(Paired {
            node_id: record.node_id,
            name: record.name,
            relays: reach.relays,
            direct: reach.direct,
        })
    }

    /// Publish a local TCP service to the nodes that `allow` names.
    pub fn expose(
        &self,
        service: &str,
        local: SocketAddr,
        allow: &[&str],
        description: &str,
    ) -> Result<(), String> {
        let allow = allow.iter().map(|rule| rule.to_string()).collect();
        self.node
            .expose(service, local, allow, description)
            .map_err(|e| e.to_string())
    }

    /// A local loopback address that reaches `service` on the node `node_id`. The address is stable: asking again gives
    /// the same one. Fails when the node cannot be reached within [`CONNECT_WAIT`].
    pub async fn connect(&self, node_id: &str, service: &str) -> Result<SocketAddr, String> {
        let attempt = self.node.connect(node_id, service);
        match tokio::time::timeout(CONNECT_WAIT, attempt).await {
            Err(_) => Err(format!(
                "the paired node {node_id} did not answer within {} seconds",
                CONNECT_WAIT.as_secs()
            )),
            Ok(Err(e)) => Err(e.to_string()),
            Ok(Ok((addr, _route))) => Ok(addr),
        }
    }

    /// The node's status, as the overlay reports it (relays, routes, services, listener).
    pub fn status(&self) -> serde_json::Value {
        self.node.status()
    }

    /// The nodes this node is paired with, with their routes.
    pub fn peers(&self) -> Vec<serde_json::Value> {
        self.node.peer_list()
    }
}

/// Reads the identity's seed, or creates one. An existing seed that is not 64 bytes is an error, never overwritten:
/// replacing it would change the node's id and break every pairing.
fn load_identity(path: &Path, name: &str) -> Result<NodeIdentity, String> {
    let seed = match std::fs::read(path) {
        Ok(bytes) => <[u8; 64]>::try_from(bytes.as_slice()).map_err(|_| {
            format!(
                "{} is not a 64-byte identity seed ({} bytes); it was not changed",
                path.display(),
                bytes.len()
            )
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut seed = [0u8; 64];
            rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut seed);
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
            file.write_all(&seed)
                .and_then(|_| file.sync_all())
                .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
            seed
        }
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    Ok(NodeIdentity::from_seed(&seed, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seed_is_created_once_and_keeps_the_same_node_id() {
        let dir = std::env::temp_dir().join(format!("mm-overlay-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(SEED_FILE);
        let first = load_identity(&path, "desk").unwrap();
        let again = load_identity(&path, "renamed").unwrap();
        assert_eq!(first.node_id(), again.node_id());
        assert_eq!(std::fs::read(&path).unwrap().len(), 64);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_damaged_seed_is_refused_and_left_alone() {
        let dir = std::env::temp_dir().join(format!("mm-overlay-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(SEED_FILE);
        std::fs::write(&path, b"short").unwrap();
        let error = load_identity(&path, "desk").err().expect("a short seed is refused");
        assert!(error.contains("not changed"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), b"short");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
