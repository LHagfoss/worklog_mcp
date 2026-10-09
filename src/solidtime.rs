//! Minimal Solidtime REST client. Only the endpoints this server needs, all
//! under `/api/v1`. Reads use `member_id` (required for `time-entries:view:own`).

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::sync::OnceCell;

/// Solidtime client. The API token is read from the shell environment.
#[derive(Clone)]
pub struct Solidtime {
    base: String,
    token: String,
    http: reqwest::Client,
    /// Cached (organization_id, member_id) resolved from `/users/me/memberships`.
    context: Arc<OnceCell<(String, String)>>,
}

impl Solidtime {
    /// Build from `SOLIDTIME_API_KEY` (fallback `SOLIDTIME_TOKEN`). Returns a
    /// clear error when no token is configured so tools can report it.
    pub fn from_env() -> Result<Self> {
        let token = ["SOLIDTIME_API_KEY", "SOLIDTIME_TOKEN", "TOKEN"]
            .iter()
            .find_map(|key| std::env::var(key).ok())
            .filter(|value| !value.trim().is_empty())
            .context(
                "Solidtime token not configured: set SOLIDTIME_API_KEY (shell env), or point WORKLOG_ENV_FILE at a .env containing TOKEN",
            )?;
        let base = std::env::var("SOLIDTIME_BASE_URL")
            .unwrap_or_else(|_| "https://solidtime.paral.no".into());
        let http = reqwest::Client::builder()
            .build()
            .context("failed to build HTTP client")?;
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
            token,
            http,
            context: Arc::new(OnceCell::new()),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    async fn handle(response: reqwest::Response, path: &str) -> Result<Value> {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!(
                "Solidtime {} returned HTTP {}: {}",
                path,
                status.as_u16(),
                text.trim()
            );
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        let owned: Vec<(String, String)> = query
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        let response = self
            .http
            .get(self.url(path))
            .bearer_auth(&self.token)
            .header("Accept", "application/json")
            .query(&owned)
            .send()
            .await
            .with_context(|| format!("request to {path} failed"))?;
        Self::handle(response, path).await
    }

    async fn write(&self, method: reqwest::Method, path: &str, body: &Value) -> Result<Value> {
        let response = self
            .http
            .request(method, self.url(path))
            .bearer_auth(&self.token)
            .header("Accept", "application/json")
            .json(body)
            .send()
            .await
            .with_context(|| format!("request to {path} failed"))?;
        Self::handle(response, path).await
    }

    /// Raw memberships list.
    pub async fn memberships(&self) -> Result<Value> {
        self.get("/api/v1/users/me/memberships", &[]).await
    }

    /// Cached (organization_id, member_id) for this user.
    pub async fn context(&self) -> Result<(String, String)> {
        let value = self
            .context
            .get_or_try_init(|| async {
                let memberships = self.memberships().await?;
                let first = memberships
                    .get("data")
                    .and_then(Value::as_array)
                    .and_then(|rows| rows.first())
                    .context("no memberships returned for this user")?;
                let member = first
                    .get("id")
                    .and_then(Value::as_str)
                    .context("membership missing id")?;
                let org = first
                    .get("organization")
                    .and_then(|o| o.get("id"))
                    .and_then(Value::as_str)
                    .context("membership missing organization id")?;
                Ok::<_, anyhow::Error>((org.to_string(), member.to_string()))
            })
            .await?;
        Ok(value.clone())
    }

    pub async fn projects(&self) -> Result<Value> {
        let (org, _) = self.context().await?;
        self.get(&format!("/api/v1/organizations/{org}/projects"), &[])
            .await
    }

    pub async fn clients(&self) -> Result<Value> {
        let (org, _) = self.context().await?;
        self.get(&format!("/api/v1/organizations/{org}/clients"), &[])
            .await
    }

    /// Time entries in [start, end) UTC, for the current member.
    pub async fn time_entries(&self, start_utc: &str, end_utc: &str) -> Result<Value> {
        let (org, member) = self.context().await?;
        self.get(
            &format!("/api/v1/organizations/{org}/time-entries"),
            &[
                ("start", start_utc.to_string()),
                ("end", end_utc.to_string()),
                ("member_id", member),
            ],
        )
        .await
    }

    /// The currently running entry, or `Ok(None)` when no timer is active.
    pub async fn active(&self) -> Result<Option<Value>> {
        let value = self.get("/api/v1/users/me/time-entries/active", &[]).await?;
        if value.is_null() {
            return Ok(None);
        }
        if let Some(data) = value.get("data") {
            if data.is_null() {
                return Ok(None);
            }
            return Ok(Some(data.clone()));
        }
        Ok(Some(value))
    }

    /// Create a time entry. Omit `end_utc` to start a running timer.
    pub async fn create_entry(
        &self,
        project_id: &str,
        start_utc: &str,
        end_utc: Option<&str>,
        description: &str,
        billable: bool,
    ) -> Result<Value> {
        let (org, member) = self.context().await?;
        let mut body = json!({
            "member_id": member,
            "start": start_utc,
            "billable": billable,
            "type": "work",
        });
        if !project_id.is_empty() {
            body["project_id"] = json!(project_id);
        }
        if let Some(end) = end_utc {
            body["end"] = json!(end);
        }
        if !description.is_empty() {
            body["description"] = json!(description);
        }
        self.write(
            reqwest::Method::POST,
            &format!("/api/v1/organizations/{org}/time-entries"),
            &body,
        )
        .await
    }

    /// Set the `end` of an entry (used to stop the running timer).
    pub async fn set_end(&self, entry_id: &str, end_utc: &str) -> Result<Value> {
        let (org, _) = self.context().await?;
        let body = json!({ "end": end_utc });
        self.write(
            reqwest::Method::PUT,
            &format!("/api/v1/organizations/{org}/time-entries/{entry_id}"),
            &body,
        )
        .await
    }
}
