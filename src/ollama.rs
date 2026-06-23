//! Minimal client for ollama-router: the model catalog (`/v1/models`) merged
//! with the currently-loaded set (`/api/ps`). Used to populate the panel's
//! model picker with a loaded indicator.

use std::time::Duration;

use serde::Deserialize;

use crate::ipc::protocol::ModelInfo;

#[derive(Debug, thiserror::Error)]
pub enum OllamaError {
    #[error("HTTP transport error: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("ollama-router returned {status}")]
    Status { status: u16 },
}

/// Client for ollama-router's catalog + loaded-model endpoints.
#[derive(Clone)]
pub struct OllamaRouterClient {
    http: reqwest::Client,
    base_url: String,
    token: Option<String>,
}

#[derive(Deserialize)]
struct V1Models {
    data: Vec<V1Model>,
}
#[derive(Deserialize)]
struct V1Model {
    id: String,
}

#[derive(Deserialize)]
struct ApiPs {
    #[serde(default)]
    models: Vec<ApiPsModel>,
}
#[derive(Deserialize)]
struct ApiPsModel {
    /// `/api/ps` carries both `name` and `model`; either may match a `/v1/models` id.
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

impl OllamaRouterClient {
    pub fn new(base_url: impl Into<String>, token: Option<String>) -> Result<Self, OllamaError> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .pool_max_idle_per_host(2)
            .build()?;
        Ok(Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token,
        })
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        let req = self
            .http
            .get(format!("{}{path}", self.base_url))
            .timeout(Duration::from_secs(10));
        match &self.token {
            Some(t) => req.bearer_auth(t),
            None => req,
        }
    }

    /// The full catalog, each flagged `loaded` if ollama-router reports it in
    /// `/api/ps`. The loaded probe is best-effort — if it fails, models are
    /// returned with `loaded: false` rather than erroring the whole call.
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, OllamaError> {
        let resp = self.get("/v1/models").send().await?;
        if !resp.status().is_success() {
            return Err(OllamaError::Status {
                status: resp.status().as_u16(),
            });
        }
        let catalog: V1Models = resp.json().await?;

        let loaded = self.loaded_set().await.unwrap_or_default();

        Ok(catalog
            .data
            .into_iter()
            .map(|m| ModelInfo {
                loaded: loaded.contains(&m.id),
                active: false,
                id: m.id,
            })
            .collect())
    }

    /// Set of currently-loaded model ids from `/api/ps`.
    async fn loaded_set(&self) -> Result<std::collections::HashSet<String>, OllamaError> {
        let resp = self.get("/api/ps").send().await?;
        if !resp.status().is_success() {
            return Err(OllamaError::Status {
                status: resp.status().as_u16(),
            });
        }
        let ps: ApiPs = resp.json().await?;
        Ok(ps
            .models
            .into_iter()
            .flat_map(|m| [m.name, m.model])
            .flatten()
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn merges_catalog_with_loaded_set() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "object": "list",
                "data": [{"id": "qwen3.6-medium"}, {"id": "gpt-oss"}, {"id": "gemma4:e4b"}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/ps"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "models": [{"name": "qwen3.6-medium", "model": "qwen3.6-medium"}]
            })))
            .mount(&server)
            .await;

        let client = OllamaRouterClient::new(server.uri(), Some("t".into())).unwrap();
        let models = client.list_models().await.unwrap();

        assert_eq!(models.len(), 3);
        let loaded: Vec<_> = models.iter().filter(|m| m.loaded).map(|m| &m.id).collect();
        assert_eq!(loaded, vec!["qwen3.6-medium"]);
        assert!(models.iter().all(|m| !m.active));
    }

    #[tokio::test]
    async fn loaded_probe_failure_degrades_to_unloaded() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{"id": "gpt-oss"}]
            })))
            .mount(&server)
            .await;
        // No /api/ps mock -> 404 -> loaded_set errors -> degrades to empty.
        let client = OllamaRouterClient::new(server.uri(), None).unwrap();
        let models = client.list_models().await.unwrap();
        assert_eq!(models.len(), 1);
        assert!(!models[0].loaded);
    }
}
