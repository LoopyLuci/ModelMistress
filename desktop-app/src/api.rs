//! The desktop app's link to the hub.
//!
//! Everything the app does goes through ModelMistress's own hub API (`model_mistress::client::Client`) with the hub's
//! token: models, loading, downloads, chat, speech, keys, usage. There is no other model server behind it. The
//! link finds the running hub through its control file; if none is running it starts one inside the app (the
//! same hub the `model-mistress` program runs), so the app works on a machine with nothing else installed.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use model_mistress::client::{default_home, Client};
use model_mistress::engine::{Engine, Settings};
use model_mistress::hub::{self, new_token, Hub, ServeOpts};
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct HubLink {
    /// None: a fixed hub that is never started or searched for (tests, or a hub the user named).
    home: Option<PathBuf>,
    client: Arc<Mutex<Option<Arc<Client>>>>,
    /// Start a hub inside the app when none answers (off once the app manages the hub process itself).
    embed: bool,
}

impl HubLink {
    /// The hub in `home` (default: MM_HOME, else the standard folder).
    pub fn new(home: Option<PathBuf>) -> Self {
        let home = home.unwrap_or_else(default_home);
        Self {
            home: Some(home),
            client: Arc::new(Mutex::new(None)),
            embed: true,
        }
    }

    /// The home this link looks in.
    pub fn home(&self) -> Option<PathBuf> {
        self.home.clone()
    }

    /// Never start a hub inside the app: the app's own lifecycle (`hub_proc`) decides that.
    pub fn without_embedded(mut self) -> Self {
        self.embed = false;
        self
    }

    /// A hub at a known address with its token.
    pub fn fixed(url: &str, token: &str) -> Self {
        Self {
            home: None,
            client: Arc::new(Mutex::new(Some(Arc::new(Client::new(url, token))))),
            embed: false,
        }
    }

    /// A client for a hub that answers, starting the app's own if none does.
    pub async fn client(&self) -> Result<Arc<Client>> {
        let mut cached = self.client.lock().await;
        if let Some(c) = cached.as_ref() {
            if c.health().await.is_ok() {
                return Ok(c.clone());
            }
            *cached = None;
        }
        let Some(home) = &self.home else {
            bail!("the hub does not answer")
        };
        let c = match Client::find(Some(home.clone())).await {
            Ok(c) => c,
            Err(e) if !self.embed => return Err(e),
            Err(_) => {
                start_embedded(home).await?;
                Client::find(Some(home.clone()))
                    .await
                    .context("the hub started inside the app does not answer")?
            }
        };
        let c = Arc::new(c);
        *cached = Some(c.clone());
        Ok(c)
    }
}

/// Run a hub in this process (as `model-mistress serve` does) and wait until it has written its control file.
pub async fn start_embedded(home: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(home)?;
    let _ = std::fs::remove_file(home.join("control.json")); // a dead hub's
    let settings = Settings::load(home).map_err(anyhow::Error::msg)?;
    let engine = {
        let home = home.to_path_buf();
        tokio::task::spawn_blocking(move || Engine::new(home, settings)).await?
    };
    let hub = Hub::new(engine, new_token());
    let opts = ServeOpts {
        home: home.to_path_buf(),
        host: [127, 0, 0, 1].into(),
        port: 0,
    };
    tokio::spawn(async move {
        if let Err(e) = hub::serve(hub, opts).await {
            eprintln!("model-mistress: the hub inside the app stopped: {e}");
        }
    });
    for _ in 0..100 {
        if home.join("control.json").exists() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!("the hub inside the app did not start within 10 seconds")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use model_mistress::testing::{self, stand_in, TOKEN};
    use serde_json::json;

    pub struct Rig {
        pub link: HubLink,
        pub hub: Hub,
        pub home: testing::TestDir,
        _task: tokio::task::JoinHandle<()>,
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            self.hub.engine.unload_all();
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }

    /// A hub in this process on a free port with models `alpha` and `beta`; `alpha` answers "A".
    pub async fn rig() -> Rig {
        let home = testing::temp_home();
        let hub = testing::hub(&home, &["alpha", "beta"]);
        let s = stand_in("A", 0).await;
        hub.engine.adopt("alpha", s.port, "k", Some(s.on_stop()));
        let (addr, task) = testing::serve(hub.clone()).await;
        Rig {
            link: HubLink::fixed(&format!("http://{addr}"), TOKEN),
            hub,
            home,
            _task: task,
        }
    }

    #[tokio::test]
    async fn the_link_talks_to_the_hub_for_everything() {
        let r = rig().await;
        let c = r.link.client().await.unwrap();
        assert_eq!(c.health().await.unwrap()["ok"], true);
        assert_eq!(c.status().await.unwrap()["models_known"], 2);
        let ids: Vec<String> = c
            .list_models(false)
            .await
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap().to_string())
            .collect();
        assert!(
            ids.contains(&"alpha".to_string()) && ids.contains(&"beta".to_string()),
            "{ids:?}"
        );
        assert_eq!(c.loaded_models().await.unwrap().len(), 1);
        // chat, plain and streamed, through the hub
        let hi = vec![json!({"role": "user", "content": "hi"})];
        assert_eq!(c.chat("alpha", hi.clone()).await.unwrap()["text"], "A");
        let mut pieces = vec![];
        let text = c
            .chat_stream("alpha", hi, |t| pieces.push(t.to_string()))
            .await
            .unwrap();
        assert_eq!((text.as_str(), pieces.len()), ("AAA", 3));
        // unloading, and a model that is not there
        assert_eq!(c.unload_model("alpha").await.unwrap()["unloaded"], 1);
        assert!(c.delete_model("no-such-model").await.is_err());
        // the wrong token is refused
        let wrong = Client::new(&c.url, "not-the-token-0123456789");
        assert!(wrong.status().await.is_err());
    }

    #[tokio::test]
    async fn the_link_manages_peer_keys() {
        let r = rig().await;
        let c = r.link.client().await.unwrap();
        let made = c
            .create_key("alice", json!({"requests_per_min": 5, "models": ["alpha"]}))
            .await
            .unwrap();
        let secret = made["key"].as_str().unwrap().to_string();
        assert_eq!(c.keys().await.unwrap().len(), 1);
        // the peer reaches the models with its key, and nothing else
        let peer = Client::new(&c.url, &secret);
        let hi = vec![json!({"role": "user", "content": "hi"})];
        assert_eq!(peer.chat_stream("alpha", hi.clone(), |_| {}).await.unwrap(), "AAA");
        assert!(
            peer.chat_stream("beta", hi, |_| {}).await.is_err(),
            "not on the allow-list"
        );
        assert!(peer.status().await.is_err(), "a peer key is not the control API");
        let usage = c.usage(Some("alice")).await.unwrap();
        assert_eq!(usage[0]["requests"], 2);
        assert_eq!(c.revoke_key("alice").await.unwrap()["revoked"], 1);
    }

    #[tokio::test]
    async fn a_link_with_no_hub_says_so() {
        let link = HubLink::fixed("http://127.0.0.1:1", TOKEN);
        assert!(link.client().await.is_err());
    }
}
