//! Client of the node's loopback API for plugins (`host_api` in
//! `host.configure`): who this node is, its live peers, a call to this plugin
//! on another node, and the node key's signature over an identity statement.

use serde_json::{json, Value};
use std::time::Duration;

const TOKEN_HEADER: &str = "x-pom-plugin-host-token";

#[derive(Clone, PartialEq, Eq)]
pub struct NodeApi {
    base_url: String,
    token: String,
}

impl std::fmt::Debug for NodeApi {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NodeApi")
            .field("base_url", &self.base_url)
            .field("token", &"<hidden>")
            .finish()
    }
}

impl NodeApi {
    /// Reads `host_api` from `host.configure`; only a loopback URL is used.
    pub fn from_configure(request: &Value) -> Option<Self> {
        let base_url = request["host_api"]["base_url"]
            .as_str()?
            .trim_end_matches('/');
        let token = request["host_api"]["token"].as_str()?;
        let loopback = ["http://127.0.0.1:", "http://localhost:", "http://[::1]:"]
            .iter()
            .any(|prefix| base_url.starts_with(prefix));
        (loopback && !token.is_empty()).then(|| Self {
            base_url: base_url.to_owned(),
            token: token.to_owned(),
        })
    }

    fn agent(timeout: Duration) -> ureq::Agent {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout(timeout)
            // The node is on loopback: no proxy may sit in between.
            .try_proxy_from_env(false)
            .build()
    }

    fn read(response: Result<ureq::Response, ureq::Error>) -> Result<Value, String> {
        match response {
            Ok(response) => serde_json::from_reader(response.into_reader())
                .map_err(|error| format!("node answered no JSON: {error}")),
            Err(ureq::Error::Status(status, response)) => {
                let body: Value =
                    serde_json::from_reader(response.into_reader()).unwrap_or_default();
                Err(body["error"]
                    .as_str()
                    .map_or_else(|| format!("node answered {status}"), str::to_owned))
            }
            Err(error) => Err(format!("node unreachable: {error}")),
        }
    }

    fn get(&self, path: &str) -> Result<Value, String> {
        Self::read(
            Self::agent(Duration::from_secs(10))
                .get(&format!("{}{path}", self.base_url))
                .set(TOKEN_HEADER, &self.token)
                .call(),
        )
    }

    fn post(&self, path: &str, body: &Value, timeout: Duration) -> Result<Value, String> {
        Self::read(
            Self::agent(timeout)
                .post(&format!("{}{path}", self.base_url))
                .set(TOKEN_HEADER, &self.token)
                .set("Content-Type", "application/json")
                .send_string(&body.to_string()),
        )
    }

    /// `{node_id, hostname, public_key, did_key, role, principal_node_id}`.
    pub fn node(&self) -> Result<Value, String> {
        self.get("/node")
    }

    /// `[{node_id, hostname, principal, visitor}]`.
    pub fn peers(&self) -> Result<Vec<Value>, String> {
        Ok(self.get("/peers")?["peers"]
            .as_array()
            .cloned()
            .unwrap_or_default())
    }

    /// `peer.<op>` of this plugin on `node_id`, through the node's signed call.
    pub fn peer(&self, node_id: &str, op: &str, params: Value) -> Result<Value, String> {
        let answer = self.post(
            "/peer",
            &json!({"node_id": node_id, "op": op, "params": params}),
            Duration::from_secs(20),
        )?;
        Ok(answer["reply"].clone())
    }

    /// Hex Ed25519 signature of the node key over `statement`.
    pub fn sign(&self, statement: &str) -> Result<String, String> {
        let answer = self.post(
            "/sign",
            &json!({"statement": statement}),
            Duration::from_secs(10),
        )?;
        answer["signature"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| "node returned no signature".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_loopback_host_api_is_accepted() {
        let configure = |url: &str| json!({"operation": "host.configure", "host_api": {"base_url": url, "token": "t"}});
        assert!(
            NodeApi::from_configure(&configure("http://127.0.0.1:8080/api/plugin-host")).is_some()
        );
        assert!(
            NodeApi::from_configure(&configure("http://10.0.0.2:8080/api/plugin-host")).is_none()
        );
        assert!(
            NodeApi::from_configure(&configure("https://example.com/api/plugin-host")).is_none()
        );
        assert!(NodeApi::from_configure(&json!({"operation": "host.configure"})).is_none());
    }

    #[test]
    fn the_token_never_shows_in_debug_output() {
        let api = NodeApi::from_configure(&json!({
            "host_api": {"base_url": "http://127.0.0.1:1/api/plugin-host", "token": "secret-token"}
        }))
        .unwrap();
        assert!(!format!("{api:?}").contains("secret-token"));
    }
}
