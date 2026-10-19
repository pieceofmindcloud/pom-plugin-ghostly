//! This node's member of the network's Ghostly group, run by an unmodified
//! Ghostly CLI. People chat in Ghostly's own web app (served by `upstream`),
//! with their profile in their browser; the node keeps a member of its own,
//! always online, proved with the node key.
//!
//! One worker thread does everything, in order: installs the runtime
//! (`crate::runtime`), keeps `ghostly daemon` running on this plugin's own
//! profile, and every few seconds reconciles this node with the network:
//!
//! - **Identity.** The profile carries a Ghostly `did` identity proof for the
//!   node's own Ed25519 key (`did:key`, the key behind its pkarr record): the
//!   CLI drafts the statement, the node signs it (`NodeApi::sign`).
//! - **The group.** The principal of the network holds one Ghostly community
//!   and hands its link, through the node's signed peer call, to the other
//!   nodes' plugins (`peer.group_link`), which join it. A node whose principal
//!   changes joins the new principal's group.
//! - **Who is who.** Each plugin answers `peer.whoami` with its member key in
//!   the group; the answers, authenticated by the node's signed transport,
//!   tie every member key to the `node_id` that holds it.
//!
//! The screen reads the result (`snapshot`) to offer the group's link to the
//! browser's profile, and asks the node to sign that profile's own `did:key`
//! statement (`sign`).

use crate::node_api::NodeApi;
use crate::runtime::{self, Runtime};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Name of the network's group in Ghostly.
const GROUP_NAME: &str = "POM network";
const COORDINATE_EVERY: Duration = Duration::from_secs(20);
const MEMBERS_EVERY: Duration = Duration::from_secs(15);
const VERIFY_EVERY: Duration = Duration::from_secs(60);
/// A proof is renewed this long before it expires.
const PROOF_RENEW_SECS: i64 = 30 * 86_400;
const CLI_TIMEOUT: Duration = Duration::from_secs(90);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
}

/// A Ghostly group id or member key as the CLI prints them.
fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// A community link: `group2/<group id>/<entry key>`.
fn valid_link(link: &str, group: &str) -> bool {
    let mut parts = link.split('/');
    parts.next() == Some("group2")
        && parts.next() == Some(group)
        && parts.next().is_some_and(valid_id)
        && parts.next().is_none()
}

#[derive(Default)]
struct Inner {
    stage: &'static str,
    step: Option<String>,
    error: Option<String>,
    version: Option<String>,
    node: Value,
    identity: Value,
    group: Option<String>,
    link: Option<String>,
    group_info: Value,
    verified: HashMap<String, Value>,
    revision: u64,
    profile_named: Option<String>,
}

pub struct Chat {
    root: PathBuf,
    inner: Mutex<Inner>,
    api: Mutex<Option<NodeApi>>,
    runtime: Mutex<Option<Runtime>>,
    daemon: Mutex<Option<Child>>,
    stop: AtomicBool,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl Chat {
    pub fn new(root: PathBuf) -> Arc<Self> {
        let saved = Saved::load(&root);
        Arc::new(Self {
            root,
            inner: Mutex::new(Inner {
                stage: "waiting",
                group: saved.as_ref().map(|saved| saved.group.clone()),
                link: saved.map(|saved| saved.link),
                ..Inner::default()
            }),
            api: Mutex::new(None),
            runtime: Mutex::new(None),
            daemon: Mutex::new(None),
            stop: AtomicBool::new(false),
            worker: Mutex::new(None),
        })
    }

    /// `host.configure`: the node's API. The worker starts on the first one.
    pub fn configure(self: &Arc<Self>, request: &Value) {
        *lock(&self.api) = NodeApi::from_configure(request);
        let mut worker = lock(&self.worker);
        if worker.is_none() {
            let chat = Arc::clone(self);
            *worker = thread::Builder::new()
                .name("pom-plugin-ghostly".into())
                .spawn(move || chat.run())
                .ok();
        }
    }

    fn api(&self) -> Option<NodeApi> {
        lock(&self.api).clone()
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn update(&self, change: impl FnOnce(&mut Inner)) {
        let mut inner = lock(&self.inner);
        change(&mut inner);
        inner.revision += 1;
    }

    fn set_stage(&self, stage: &'static str, error: Option<String>) {
        self.update(|inner| {
            inner.stage = stage;
            inner.error = error;
        });
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    fn sleep(&self, duration: Duration) {
        let deadline = Instant::now() + duration;
        while !self.stopped() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn run(self: Arc<Self>) {
        let _ = fs::create_dir_all(&self.root);
        let mut next_coordinate = Instant::now();
        let mut next_members = Instant::now();
        let mut next_verify = Instant::now();
        while !self.stopped() {
            let Some(api) = self.api() else {
                self.set_stage("waiting", None);
                self.sleep(Duration::from_secs(1));
                continue;
            };
            if lock(&self.runtime).is_none() {
                self.set_stage("installing", None);
                let report = |step: &str| {
                    let step = step.to_owned();
                    self.update(|inner| inner.step = Some(step));
                };
                match runtime::install(&self.root, &report) {
                    Ok(runtime) => {
                        let version = runtime.version.clone();
                        *lock(&self.runtime) = Some(runtime);
                        self.update(|inner| {
                            inner.version = Some(version);
                            inner.step = None;
                        });
                    }
                    Err(error) => {
                        self.set_stage("error", Some(error));
                        self.sleep(Duration::from_secs(60));
                        continue;
                    }
                }
            }
            if let Err(error) = self.ensure_daemon() {
                self.set_stage("error", Some(error));
                self.sleep(Duration::from_secs(10));
                continue;
            }
            let now = Instant::now();
            if now >= next_coordinate {
                next_coordinate = now + COORDINATE_EVERY;
                match self.coordinate(&api) {
                    Ok(()) => {
                        if lock(&self.inner).stage != "ready" {
                            self.set_stage("ready", None);
                        }
                    }
                    Err(error) => self.set_stage("joining", Some(error)),
                }
            }
            if now >= next_members {
                next_members = now + MEMBERS_EVERY;
                self.refresh_group();
            }
            if now >= next_verify {
                next_verify = now + VERIFY_EVERY;
                self.verify_members(&api);
            }
            self.sleep(Duration::from_secs(2));
        }
    }

    /// Runs the CLI and returns its JSON answer; an `{"error": …}` answer is an error.
    fn cli(&self, args: &[&str], input: Option<&str>) -> Result<Value, String> {
        self.run_cli(args, input, true)
    }

    /// `cancellable`: a stop of the plugin ends the command early.
    fn run_cli(
        &self,
        args: &[&str],
        input: Option<&str>,
        cancellable: bool,
    ) -> Result<Value, String> {
        let runtime = lock(&self.runtime)
            .clone()
            .ok_or("the Ghostly runtime is not installed yet")?;
        let mut command = runtime.command(&self.home());
        command
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if input.is_some() {
            command.stdin(Stdio::piped());
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("run ghostly: {error}"))?;
        if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
            let _ = stdin.write_all(input.as_bytes());
        }
        let mut stdout = child.stdout.take().ok_or("ghostly has no output")?;
        let reader = thread::spawn(move || {
            let mut output = String::new();
            let _ = stdout.read_to_string(&mut output);
            output
        });
        let deadline = Instant::now() + CLI_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if (cancellable && self.stopped()) || Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "ghostly {} did not finish",
                        args.first().unwrap_or(&"")
                    ));
                }
                Ok(None) => thread::sleep(Duration::from_millis(50)),
                Err(error) => return Err(format!("ghostly: {error}")),
            }
        }
        let output = reader.join().unwrap_or_default();
        let line = output
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .ok_or_else(|| format!("ghostly {} answered nothing", args.first().unwrap_or(&"")))?;
        let answer: Value = serde_json::from_str(line)
            .map_err(|error| format!("ghostly answered no JSON: {error}"))?;
        if let Some(error) = answer.get("error") {
            return Err(error["message"]
                .as_str()
                .or_else(|| error.as_str())
                .unwrap_or("ghostly refused")
                .to_owned());
        }
        Ok(answer)
    }

    fn daemon_running(&self) -> bool {
        self.cli(&["daemon", "status"], None)
            .is_ok_and(|status| status["running"].as_bool() == Some(true))
    }

    /// Keeps `ghostly daemon` running. A daemon left by an earlier run of the
    /// plugin is adopted rather than started twice.
    fn ensure_daemon(&self) -> Result<(), String> {
        {
            let mut daemon = lock(&self.daemon);
            if let Some(child) = daemon.as_mut() {
                if matches!(child.try_wait(), Ok(None)) {
                    return Ok(());
                }
                *daemon = None;
            }
        }
        if self.daemon_running() {
            return Ok(());
        }
        self.set_stage("starting", None);
        let runtime = lock(&self.runtime).clone().ok_or("no runtime")?;
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join("daemon.log"))
            .map_err(|error| format!("daemon log: {error}"))?;
        let errors = log
            .try_clone()
            .map_err(|error| format!("daemon log: {error}"))?;
        let child = runtime
            .command(&self.home())
            .arg("daemon")
            .stdout(log)
            .stderr(errors)
            .spawn()
            .map_err(|error| format!("start ghostly daemon: {error}"))?;
        *lock(&self.daemon) = Some(child);
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline && !self.stopped() {
            if self.daemon_running() {
                return Ok(());
            }
            self.sleep(Duration::from_millis(500));
        }
        Err("the Ghostly daemon did not start".into())
    }

    fn coordinate(&self, api: &NodeApi) -> Result<(), String> {
        let node = api.node()?;
        self.update(|inner| inner.node = node.clone());
        let hostname = node["hostname"].as_str().unwrap_or_default();
        let name = if hostname.is_empty() {
            let node_id = node["node_id"].as_str().unwrap_or_default();
            format!("POM {}", node_id.chars().take(8).collect::<String>())
        } else {
            hostname.chars().take(64).collect()
        };
        if lock(&self.inner).profile_named.as_deref() != Some(name.as_str()) {
            self.cli(&["profile", "set", "--name", &name], None)?;
            self.update(|inner| inner.profile_named = Some(name));
        }
        if let Some(did_key) = node["did_key"].as_str() {
            if let Err(error) = self.ensure_identity(api, did_key) {
                self.update(|inner| inner.identity = json!({"did_key": did_key, "error": error}));
            }
        }
        let own = node["node_id"].as_str().unwrap_or_default();
        let principal = node["principal_node_id"].as_str().unwrap_or_default();
        if node["role"] == "principal" || principal.is_empty() || principal == own {
            self.serve_group()
        } else {
            self.follow_principal(api, principal)
        }
    }

    /// The profile's `did` proof for the node key, made or renewed when needed.
    fn ensure_identity(&self, api: &NodeApi, did_key: &str) -> Result<(), String> {
        let list = self.cli(&["identity", "list"], None)?;
        let current = list["proofs"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|proof| {
                proof["provider"] == "did"
                    && proof["subject"] == did_key
                    && proof["expiresAt"].as_i64().unwrap_or(0) > unix_now() + PROOF_RENEW_SECS
            });
        if let Some(proof) = current {
            let summary = json!({
                "did_key": did_key,
                "proof": proof["id"],
                "expires_at": proof["expiresAt"],
                "source": proof["verified"]["source"],
            });
            self.update(|inner| inner.identity = summary);
            return Ok(());
        }
        let draft = self.cli(&["identity", "add", "did", did_key, "--days", "365"], None)?;
        let (Some(id), Some(statement)) = (draft["draft"].as_str(), draft["statement"].as_str())
        else {
            return Err("ghostly made no identity draft".into());
        };
        let signature = match api.sign(statement) {
            Ok(signature) => signature,
            Err(error) => {
                let _ = self.cli(&["identity", "cancel", id], None);
                return Err(error);
            }
        };
        self.cli(&["identity", "complete", id, "--stdin"], Some(&signature))?;
        Ok(())
    }

    fn save(&self, group: &str, link: &str) {
        Saved {
            group: group.to_owned(),
            link: link.to_owned(),
        }
        .store(&self.root);
        self.update(|inner| {
            if inner.group.as_deref() != Some(group) {
                inner.verified.clear();
                inner.group_info = Value::Null;
            }
            inner.group = Some(group.to_owned());
            inner.link = Some(link.to_owned());
        });
    }

    fn group_alive(&self, group: &str) -> bool {
        self.cli(&["group", "show", group], None)
            .is_ok_and(|show| !matches!(show["status"].as_str(), Some("removed" | "left") | None))
    }

    /// On the principal: the network's group, created once.
    fn serve_group(&self) -> Result<(), String> {
        let (group, link) = {
            let inner = lock(&self.inner);
            (inner.group.clone(), inner.link.clone())
        };
        if let (Some(group), Some(_)) = (&group, &link) {
            if self.group_alive(group) {
                return Ok(());
            }
        }
        let created = self.cli(&["group", "create", GROUP_NAME], None)?;
        let (Some(group), Some(link)) = (created["group"].as_str(), created["link"].as_str())
        else {
            return Err("ghostly created no group link".into());
        };
        if !valid_id(group) || !valid_link(link, group) {
            return Err("ghostly answered an unexpected group link".into());
        }
        self.save(group, link);
        Ok(())
    }

    /// On any other node: the principal's group, joined through its link.
    fn follow_principal(&self, api: &NodeApi, principal: &str) -> Result<(), String> {
        let reply = api
            .peer(principal, "group_link", Value::Null)
            .map_err(|error| format!("the principal's Ghostly plugin did not answer: {error}"))?;
        let (Some(group), Some(link)) = (reply["group"].as_str(), reply["link"].as_str()) else {
            return Err(reply["error"]
                .as_str()
                .unwrap_or("the principal has no Ghostly group yet")
                .to_owned());
        };
        if !valid_id(group) || !valid_link(link, group) {
            return Err("the principal answered an unexpected group link".into());
        }
        let current = lock(&self.inner).group.clone();
        if current.as_deref() == Some(group) && self.group_alive(group) {
            return Ok(());
        }
        if let Some(old) = current.filter(|old| old != group) {
            // The network moved to another principal's group.
            let _ = self.cli(&["group", "leave", &old, "--yes"], None);
        }
        self.cli(&["group", "join", "-"], Some(link))?;
        self.save(group, link);
        Ok(())
    }

    fn refresh_group(&self) {
        let Some(group) = lock(&self.inner).group.clone() else {
            return;
        };
        if let Ok(show) = self.cli(&["group", "show", &group], None) {
            self.update(|inner| inner.group_info = show);
        }
    }

    /// Ties member keys to nodes: each live peer's plugin says which member it
    /// is, over the node's signed call, so the answer is that node's.
    fn verify_members(&self, api: &NodeApi) {
        let (group, me, node) = {
            let inner = lock(&self.inner);
            (
                inner.group.clone(),
                inner.group_info["me"].as_str().map(str::to_owned),
                inner.node.clone(),
            )
        };
        let Some(group) = group else {
            return;
        };
        let mut verified = HashMap::new();
        if let Some(me) = me {
            verified.insert(
                me,
                json!({
                    "node_id": node["node_id"],
                    "hostname": node["hostname"],
                    "principal": node["role"] == "principal",
                    "me": true,
                }),
            );
        }
        for peer in api.peers().unwrap_or_default() {
            let Some(node_id) = peer["node_id"].as_str() else {
                continue;
            };
            let Ok(reply) = api.peer(node_id, "whoami", Value::Null) else {
                continue;
            };
            if reply["group"].as_str() != Some(group.as_str()) {
                continue;
            }
            if let Some(member) = reply["member"].as_str().filter(|member| valid_id(member)) {
                verified.insert(
                    member.to_owned(),
                    json!({
                        "node_id": node_id,
                        "hostname": peer["hostname"],
                        "principal": peer["principal"],
                        "visitor": peer["visitor"],
                    }),
                );
            }
        }
        self.update(|inner| inner.verified = verified);
    }

    /// What the screen shows.
    pub fn snapshot(&self) -> Value {
        let inner = lock(&self.inner);
        let members: Vec<Value> = inner.group_info["members"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|member| {
                let key = member["key"].as_str().unwrap_or_default();
                json!({
                    "key": key,
                    "name": member["name"],
                    "role": member["role"],
                    "me": member["me"],
                    "online": member["online"],
                    "node": inner.verified.get(key).cloned().unwrap_or(Value::Null),
                })
            })
            .collect();
        json!({
            "stage": inner.stage,
            "step": inner.step,
            "error": inner.error,
            "version": inner.version,
            "node": {
                "node_id": inner.node["node_id"],
                "hostname": inner.node["hostname"],
                "did_key": inner.node["did_key"],
                "role": inner.node["role"],
            },
            "identity": inner.identity,
            "group": inner.group.as_ref().map(|id| json!({
                "id": id,
                "link": inner.link,
                "name": inner.group_info["name"],
                "status": inner.group_info["status"],
                "me": inner.group_info["me"],
                "members": members,
            })),
            "revision": inner.revision,
        })
    }

    /// `host.services`: the chat as a service of this machine.
    pub fn service(&self) -> Value {
        let inner = lock(&self.inner);
        let status = match inner.stage {
            "ready" => "ready",
            "installing" => "installing",
            "error" => "error",
            "waiting" => "waiting",
            _ => "starting",
        };
        let mut service =
            json!({"id": "ghostly", "kind": "chat", "name": "Ghostly", "status": status});
        if let Some(version) = &inner.version {
            service["version"] = json!(version);
        }
        if let Some(error) = &inner.error {
            service["detail"] = json!(error.chars().take(160).collect::<String>());
        }
        json!({"services": [service]})
    }

    /// `peer.group_link`: the network's group, for another node of the network
    /// (the node already proved the caller is a live peer).
    pub fn group_link(&self) -> Value {
        let inner = lock(&self.inner);
        match (&inner.group, &inner.link) {
            (Some(group), Some(link)) => json!({"group": group, "link": link}),
            _ => json!({"error": "no Ghostly group on this node yet"}),
        }
    }

    /// The built Ghostly web app, once installed.
    pub fn web_root(&self) -> Option<PathBuf> {
        lock(&self.runtime)
            .as_ref()
            .map(|runtime| runtime.web.clone())
    }

    /// The node key's signature over a browser profile's identity statement.
    /// The node signs only a statement for its own `did:key`.
    pub fn sign(&self, statement: &str) -> Result<Value, String> {
        let api = self
            .api()
            .ok_or("the node has not configured the plugin yet")?;
        let signature = api.sign(statement)?;
        Ok(json!({"signature": signature}))
    }

    /// `peer.whoami`: this node's member key in the group.
    pub fn whoami(&self) -> Value {
        let inner = lock(&self.inner);
        json!({"group": inner.group, "member": inner.group_info["me"]})
    }

    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = lock(&self.worker).take() {
            let _ = worker.join();
        }
        if let Some(mut child) = lock(&self.daemon).take() {
            // Tell the contacts it is going, then make sure it went.
            let _ = self.run_cli(&["daemon", "stop"], None, false);
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline && matches!(child.try_wait(), Ok(None)) {
                thread::sleep(Duration::from_millis(100));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The group this node belongs to, kept across restarts.
struct Saved {
    group: String,
    link: String,
}

impl Saved {
    fn path(root: &Path) -> PathBuf {
        root.join("network-group.json")
    }

    fn load(root: &Path) -> Option<Self> {
        let value: Value = serde_json::from_slice(&fs::read(Self::path(root)).ok()?).ok()?;
        let group = value["group"].as_str()?.to_owned();
        let link = value["link"].as_str()?.to_owned();
        (valid_id(&group) && valid_link(&link, &group)).then_some(Self { group, link })
    }

    fn store(&self, root: &Path) {
        let _ = fs::create_dir_all(root);
        let path = Self::path(root);
        let partial = path.with_extension("json.partial");
        let body = json!({"group": self.group, "link": self.link}).to_string();
        if fs::write(&partial, body).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&partial, fs::Permissions::from_mode(0o600));
            }
            let _ = fs::rename(&partial, &path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat() -> Arc<Chat> {
        let root = std::env::temp_dir().join(format!(
            "pom-plugin-ghostly-chat-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        Chat::new(root)
    }

    #[test]
    fn only_a_community_link_of_the_named_group_is_accepted() {
        let group = "JMSWW-nd7sHQg44HBcrquA";
        assert!(valid_link(
            &format!("group2/{group}/nizxrzjd3m4gtum8azm7z7j9g1iyg4s8p7p7w9sfuduk4iujmyro"),
            group
        ));
        assert!(!valid_link(&format!("group1/{group}/abc"), group));
        assert!(!valid_link("group2/other/abc", group));
        assert!(!valid_link(&format!("group2/{group}/abc/def"), group));
        assert!(!valid_link(&format!("group2/{group}/a b"), group));
    }

    #[test]
    fn the_saved_group_survives_a_restart() {
        let first = chat();
        let link = "group2/abc_DEF-1/key123";
        first.save("abc_DEF-1", link);
        let second = Chat::new(first.root.clone());
        assert_eq!(
            second.group_link(),
            json!({"group": "abc_DEF-1", "link": link})
        );
        fs::remove_dir_all(&first.root).unwrap();
    }

    #[test]
    fn the_service_follows_the_stage() {
        let chat = chat();
        assert_eq!(chat.service()["services"][0]["status"], "waiting");
        chat.set_stage("error", Some("offline".into()));
        assert_eq!(chat.service()["services"][0]["status"], "error");
        assert_eq!(chat.service()["services"][0]["detail"], "offline");
    }
}
