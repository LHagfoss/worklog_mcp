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
    let value: Value =
        serde_json::from_slice(&stdout).context("glab projects returned non-JSON")?;
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
pub async fn multica_issues(workspace_id: &str, limit: u32, offset: u32) -> Result<Value> {
    let limit = limit.to_string();
    let offset = offset.to_string();
    let mut args: Vec<&str> = Vec::new();
    if !workspace_id.is_empty() {
        args.push("--workspace-id");
        args.push(workspace_id);
    }
    args.extend([
        "issue", "list", "--output", "json", "--limit", &limit, "--offset", &offset,
    ]);
    let stdout = run("multica", &args).await?;
    serde_json::from_slice(&stdout)
        .context("multica issue list returned non-JSON (check `multica auth status`)")
}

/// Longest string kept per field in Multica output (descriptions can be huge).
const MAX_TEXT_CHARS: usize = 300;
/// Upper bound on the serialized issue page, so one call cannot flood context.
const MAX_PAGE_BYTES: usize = 24_000;

fn shorten_strings(value: &mut Value) {
    match value {
        Value::String(text) if text.chars().count() > MAX_TEXT_CHARS => {
            let extra = text.chars().count() - MAX_TEXT_CHARS;
            let kept: String = text.chars().take(MAX_TEXT_CHARS).collect();
            *text = format!("{kept}…[+{extra} chars]");
        }
        Value::Array(items) => items.iter_mut().for_each(shorten_strings),
        Value::Object(map) => map.values_mut().for_each(shorten_strings),
        _ => {}
    }
}

/// Normalize a `multica issue list --output json` payload into a bounded page:
/// `{offset, limit, returned, next_offset, truncated?, issues}`. Accepts either
/// a bare array or an object wrapping the issue array.
pub fn compact_issue_page(value: Value, limit: u32, offset: u32) -> Value {
    let mut issues = match value {
        Value::Array(items) => items,
        Value::Object(mut map) => {
            let key = ["issues", "data", "items"]
                .into_iter()
                .find(|k| map.get(*k).is_some_and(Value::is_array))
                .map(str::to_string)
                .or_else(|| {
                    map.iter()
                        .find(|(_, v)| v.is_array())
                        .map(|(k, _)| k.clone())
                });
            match key.and_then(|k| map.remove(&k)) {
                Some(Value::Array(items)) => items,
                _ => vec![Value::Object(map)],
            }
        }
        other => vec![other],
    };
    issues.iter_mut().for_each(shorten_strings);
    let fetched = issues.len();
    let mut truncated = false;
    while issues.len() > 1 && serde_json::to_string(&issues).map_or(0, |s| s.len()) > MAX_PAGE_BYTES
    {
        issues.pop();
        truncated = true;
    }
    let returned = issues.len();
    let next_offset = (truncated || fetched >= limit as usize).then(|| offset as usize + returned);
    let mut page = serde_json::json!({
        "offset": offset,
        "limit": limit,
        "returned": returned,
        "next_offset": next_offset,
        "issues": issues,
    });
    if truncated {
        page["truncated"] = Value::String(format!(
            "page cut to {returned} of {fetched} issues to stay under {MAX_PAGE_BYTES} bytes"
        ));
    }
    page
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn issue_page_shortens_text_and_reports_next_offset() {
        let long = "x".repeat(1000);
        let raw = json!({ "issues": [{ "title": "a", "description": long }], "total": 9 });
        let page = compact_issue_page(raw, 1, 4);
        assert_eq!(page["returned"], 1);
        assert_eq!(page["next_offset"], 5);
        let description = page["issues"][0]["description"].as_str().unwrap();
        assert!(description.ends_with("…[+700 chars]"));
        assert!(page.get("truncated").is_none());
    }

    #[test]
    fn issue_page_is_bounded_in_size() {
        let issues: Vec<Value> = (0..200)
            .map(|i| json!({ "id": i, "description": "y".repeat(300) }))
            .collect();
        let page = compact_issue_page(Value::Array(issues), 100, 0);
        assert!(serde_json::to_string(&page["issues"]).unwrap().len() <= MAX_PAGE_BYTES);
        assert!(page["truncated"].is_string());
        assert_eq!(page["next_offset"], page["returned"]);
    }

    #[test]
    fn short_last_page_has_no_next_offset() {
        let page = compact_issue_page(json!([{ "id": 1 }]), 20, 0);
        assert!(page["next_offset"].is_null());
    }
}
