//! Read-only activity providers. GitLab is the primary work signal; Multica is
//! a supplemental one. Both shell out to the already-authenticated CLIs
//! (`glab`, `multica`) and never mutate anything.

use anyhow::{Context, Result, bail};
use serde_json::Value;

/// Default GitLab host for this user.
pub const DEFAULT_GITLAB_HOST: &str = "git.paral.no";

/// Run a command, returning stdout, or a readable error including stderr.
async fn run(program: &str, args: &[&str]) -> Result<Vec<u8>> {
    let output = tokio::process::Command::new(program)
        .args(args)
        .output()
        .await
        .with_context(|| format!("failed to run `{program}` (is it installed and on PATH?)"))?;
    if !output.status.success() {
        bail!(
            "`{program}` exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

/// Current GitLab user's recent events (newest first).
pub async fn gitlab_events(host: &str, per_page: u32) -> Result<Vec<Value>> {
    let range = format!("events?per_page={per_page}");
    let stdout = run("glab", &["api", &range, "--hostname", host]).await?;
    let value: Value = serde_json::from_slice(&stdout)
        .context("glab events returned non-JSON (check `glab auth status`)")?;
    Ok(value.as_array().cloned().unwrap_or_default())
}

/// Map of GitLab project id (as string) -> full path_with_namespace.
pub async fn gitlab_project_map(host: &str) -> Result<std::collections::HashMap<String, String>> {
    let stdout = run(
        "glab",
        &[
            "api",
            "projects?membership=true&per_page=100&simple=true",
            "--hostname",
            host,
        ],
    )
    .await?;
    let value: Value = serde_json::from_slice(&stdout).context("glab projects returned non-JSON")?;
    let mut map = std::collections::HashMap::new();
    if let Some(items) = value.as_array() {
        for item in items {
            if let (Some(id), Some(path)) = (
                item.get("id").and_then(Value::as_i64),
                item.get("path_with_namespace").and_then(Value::as_str),
            ) {
                map.insert(id.to_string(), path.to_string());
            }
        }
    }
    Ok(map)
}

/// List Multica issues for a workspace (read-only).
pub async fn multica_issues(workspace_id: &str, limit: u32) -> Result<Value> {
    let limit = limit.to_string();
    let mut args: Vec<&str> = Vec::new();
    if !workspace_id.is_empty() {
        args.push("--workspace-id");
        args.push(workspace_id);
    }
    args.extend(["issue", "list", "--output", "json", "--limit", &limit]);
    let stdout = run("multica", &args).await?;
    serde_json::from_slice(&stdout)
        .context("multica issue list returned non-JSON (check `multica auth status`)")
}
