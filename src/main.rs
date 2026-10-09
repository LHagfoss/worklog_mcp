//! worklog_mcp — a local stdio MCP server for time logging.
//!
//! It exposes typed tools over the Solidtime REST API (Solidtime has no official
//! MCP), plus read-only GitLab/Multica activity signals and glue tools that help
//! an agent reconcile logged hours with real work. It shells out to the
//! already-authenticated `glab` and `multica` CLIs for activity only.

mod activity;
mod helpers;
mod solidtime;
mod timeutil;
mod tools;

use rmcp::{
    ErrorData as McpError, ServiceExt,
    model::{CallToolResult, ContentBlock},
    transport::stdio,
};
use serde_json::Value;

use solidtime::Solidtime;

/// Wrap any error as an MCP internal error.
pub fn err(message: impl std::fmt::Display) -> McpError {
    McpError::internal_error(message.to_string(), None)
}

/// Return a pretty-printed JSON tool result.
pub fn ok(value: Value) -> Result<CallToolResult, McpError> {
    let text = serde_json::to_string_pretty(&value).map_err(err)?;
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load secrets from WORKLOG_ENV_FILE when set, otherwise a `.env` in the
    // working directory or an ancestor. Existing shell variables always win.
    match std::env::var("WORKLOG_ENV_FILE") {
        Ok(path) if !path.is_empty() => {
            let _ = dotenvy::from_path(&path);
        }
        _ => {
            dotenvy::dotenv().ok();
        }
    }
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let client = Solidtime::from_env().unwrap_or_else(|e| {
        eprintln!("worklog_mcp: {e}");
        std::process::exit(1);
    });
    eprintln!(
        "worklog_mcp: base {} (timezone Europe/Oslo, GitLab {})",
        std::env::var("SOLIDTIME_BASE_URL").unwrap_or_else(|_| "https://solidtime.paral.no".into()),
        activity::DEFAULT_GITLAB_HOST,
    );

    let service = tools::WorklogMcp::new(client).serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
