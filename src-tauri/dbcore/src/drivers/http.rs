use crate::auth::{self, TokenSource};
use crate::model::ConnectionConfig;
use anyhow::{bail, Result};
use reqwest::Method;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// Small JSON-over-HTTP client shared by ArangoDB, ClickHouse and Elasticsearch.
pub struct Http {
    client: reqwest::Client,
    pub base: String,
    user: String,
    pass: String,
    /// Entra ID bearer tokens instead of basic auth.
    entra: Option<Arc<TokenSource>>,
}

pub enum Body {
    None,
    Json(Value),
    Text(String, &'static str),
}

impl Http {
    pub fn new(cfg: &ConnectionConfig, default_port: u16) -> Result<Self> {
        let base = if cfg.has_url() {
            cfg.url.trim().trim_end_matches('/').to_string()
        } else {
            let scheme = if cfg.ssl { "https" } else { "http" };
            format!("{scheme}://{}:{}", cfg.host_or_default(), cfg.port_or(default_port))
        };
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(cfg.trust_cert)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(600))
            .build()?;
        let entra = auth::token_source(cfg)?;
        Ok(Self { client, base, user: cfg.user.clone(), pass: cfg.password.clone(), entra })
    }

    pub async fn send(&self, method: Method, path: &str, body: Body) -> Result<(u16, String)> {
        let mut req = self.client.request(method, format!("{}{}", self.base, path));
        if let Some(src) = &self.entra {
            req = req.bearer_auth(src.token().await?.secret);
        } else if !self.user.is_empty() {
            req = req.basic_auth(&self.user, Some(&self.pass));
        }
        req = match body {
            Body::None => req,
            Body::Json(v) => req.json(&v),
            Body::Text(t, content_type) => req.header("Content-Type", content_type).body(t),
        };
        let res = req.send().await?;
        let status = res.status().as_u16();
        Ok((status, res.text().await?))
    }

    /// Sends a request and parses JSON, turning error payloads into errors.
    pub async fn json(&self, method: Method, path: &str, body: Body) -> Result<Value> {
        let (status, text) = self.send(method, path, body).await?;
        let parsed: Option<Value> = serde_json::from_str(&text).ok();
        if !(200..300).contains(&status) || parsed.as_ref().and_then(|v| v.get("error")).and_then(Value::as_bool) == Some(true) {
            let msg = parsed
                .as_ref()
                .and_then(|v| {
                    v.get("errorMessage")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or_else(|| v.pointer("/error/root_cause/0/reason").and_then(Value::as_str).map(str::to_string))
                        .or_else(|| v.pointer("/error/reason").and_then(Value::as_str).map(str::to_string))
                        .or_else(|| v.get("error").and_then(Value::as_str).map(str::to_string))
                })
                .unwrap_or_else(|| text.chars().take(2000).collect());
            bail!("HTTP {status}: {msg}");
        }
        match parsed {
            Some(v) => Ok(v),
            None => Ok(Value::String(text)),
        }
    }
}
