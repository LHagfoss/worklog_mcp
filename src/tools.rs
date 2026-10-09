//! MCP tool definitions. Two namespaces:
//!  * `solidtime_*` — typed Solidtime reads and writes.
//!  * `activity_*` / `worklog_*` — read-only GitLab/Multica signals and glue.

use std::collections::BTreeMap;

use rmcp::{
    ErrorData as McpError,
    handler::server::wrapper::Parameters,
    model::CallToolResult,
    schemars, tool, tool_router,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::activity;
use crate::err;
use crate::helpers::{data_array, entries_between, events_for_day};
use crate::ok;
use crate::solidtime::Solidtime;
use crate::timeutil as t;

#[derive(Clone)]
pub struct WorklogMcp {
    client: Solidtime,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct EmptyParams {}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DateParams {
    /// Oslo-local date (YYYY-MM-DD). Defaults to today when omitted.
    date: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RangeParams {
    /// Inclusive start date (YYYY-MM-DD), Oslo-local.
    start_date: String,
    /// Inclusive end date (YYYY-MM-DD), Oslo-local.
    end_date: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct StartTimerParams {
    /// Solidtime project UUID. Optional — omit to start on "No project".
    project_id: Option<String>,
    /// Free-text description.
    description: Option<String>,
    /// Whether the entry is billable. Defaults to true.
    billable: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CreateEntryParams {
    /// Oslo-local date (YYYY-MM-DD) for the block.
    date: String,
    /// Oslo-local start time (HH:MM).
    start: String,
    /// Oslo-local end time (HH:MM).
    end: String,
    /// Solidtime project UUID. Optional.
    project_id: Option<String>,
    /// Free-text description.
    description: Option<String>,
    /// Whether the entry is billable. Defaults to true.
    billable: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GitlabParams {
    /// Oslo-local date (YYYY-MM-DD) to filter events to. Defaults to today.
    date: Option<String>,
    /// How many recent events to fetch before filtering. Defaults to 100.
    per_page: Option<u32>,
    /// GitLab hostname. Defaults to git.paral.no.
    host: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct MulticaParams {
    /// Multica workspace UUID. Defaults to the configured workspace.
    workspace_id: Option<String>,
    /// Maximum issues to list. Defaults to 50.
    limit: Option<u32>,
}

impl WorklogMcp {
    pub fn new(client: Solidtime) -> Self {
        Self { client }
    }

    /// Resolve an optional date argument to an Oslo-local date.
    fn resolve_date(&self, date: Option<&str>) -> Result<chrono::NaiveDate, McpError> {
        match date {
            Some(value) => t::parse_date(value).map_err(err),
            None => Ok(t::today_local()),
        }
    }

    fn host<'a>(&self, host: Option<&'a str>) -> &'a str {
        host.unwrap_or(activity::DEFAULT_GITLAB_HOST)
    }
}

#[tool_router(server_handler)]
impl WorklogMcp {
    #[tool(
        description = "Solidtime: return the authenticated user and their organization/member IDs. Use this first to confirm connectivity and IDs."
    )]
    async fn solidtime_whoami(
        &self,
        Parameters(_): Parameters<EmptyParams>,
    ) -> Result<CallToolResult, McpError> {
        let memberships = self.client.memberships().await.map_err(err)?;
        let (org, member) = self.client.context().await.map_err(err)?;
        ok(json!({
            "memberships": memberships.get("data").cloned().unwrap_or(Value::Null),
            "organization_id": org,
            "member_id": member,
            "base_url": std::env::var("SOLIDTIME_BASE_URL")
                .unwrap_or_else(|_| "https://solidtime.paral.no".into()),
            "timezone": "Europe/Oslo",
        }))
    }

    #[tool(
        description = "Solidtime: list the organization's projects with id, name, client_id and archived flag. Use the `id` (UUID) when creating entries."
    )]
    async fn solidtime_list_projects(
        &self,
        Parameters(_): Parameters<EmptyParams>,
    ) -> Result<CallToolResult, McpError> {
        let projects = self.client.projects().await.map_err(err)?;
        let rows: Vec<Value> = data_array(&projects)
            .iter()
            .map(|p| {
                json!({
                    "id": p.get("id").cloned().unwrap_or(Value::Null),
                    "name": p.get("name").cloned().unwrap_or(Value::Null),
                    "client_id": p.get("client_id").cloned().unwrap_or(Value::Null),
                    "is_archived": p.get("is_archived").cloned().unwrap_or(json!(false)),
                })
            })
            .collect();
        ok(json!({ "projects": rows }))
    }

    #[tool(
        description = "Solidtime: list the organization's clients with id, name and archived flag."
    )]
    async fn solidtime_list_clients(
        &self,
        Parameters(_): Parameters<EmptyParams>,
    ) -> Result<CallToolResult, McpError> {
        let clients = self.client.clients().await.map_err(err)?;
        let rows: Vec<Value> = data_array(&clients)
            .iter()
            .map(|c| {
                json!({
                    "id": c.get("id").cloned().unwrap_or(Value::Null),
                    "name": c.get("name").cloned().unwrap_or(Value::Null),
                    "is_archived": c.get("is_archived").cloned().unwrap_or(json!(false)),
                })
            })
            .collect();
        ok(json!({ "clients": rows }))
    }

    #[tool(
        description = "Solidtime: list time entries for a single Oslo-local day (YYYY-MM-DD, defaults to today), with Oslo-local times, project names, descriptions and the day total."
    )]
    async fn solidtime_list_entries(
        &self,
        Parameters(p): Parameters<DateParams>,
    ) -> Result<CallToolResult, McpError> {
        let date = self.resolve_date(p.date.as_deref())?;
        ok(entries_between(&self.client, date, date).await?)
    }

    #[tool(
        description = "Solidtime: list time entries across an inclusive Oslo-local date range (start_date..end_date) with per-day totals."
    )]
    async fn solidtime_list_entries_range(
        &self,
        Parameters(p): Parameters<RangeParams>,
    ) -> Result<CallToolResult, McpError> {
        let from = t::parse_date(&p.start_date).map_err(err)?;
        let to = t::parse_date(&p.end_date).map_err(err)?;
        ok(entries_between(&self.client, from, to).await?)
    }

    #[tool(
        description = "Solidtime: return the currently running time entry, or running=false when no timer is active."
    )]
    async fn solidtime_get_active_timer(
        &self,
        Parameters(_): Parameters<EmptyParams>,
    ) -> Result<CallToolResult, McpError> {
        match self.client.active().await.map_err(err)? {
            Some(entry) => ok(json!({ "running": true, "entry": entry })),
            None => ok(json!({ "running": false, "entry": null })),
        }
    }

    #[tool(
        description = "Solidtime: start a running timer now. Optional project_id and description. Fails if a timer is already running (stop it first)."
    )]
    async fn solidtime_start_timer(
        &self,
        Parameters(p): Parameters<StartTimerParams>,
    ) -> Result<CallToolResult, McpError> {
        if self.client.active().await.map_err(err)?.is_some() {
            return Err(err("a timer is already running; stop it first"));
        }
        let now = chrono::Utc::now();
        let entry = self
            .client
            .create_entry(
                p.project_id.as_deref().unwrap_or(""),
                &t::fmt_utc(now),
                None,
                p.description.as_deref().unwrap_or(""),
                p.billable.unwrap_or(true),
            )
            .await
            .map_err(err)?;
        ok(json!({ "started": true, "start_local": t::fmt_local_hm(now), "entry": entry }))
    }

    #[tool(
        description = "Solidtime: stop the running timer by setting its end to now. Errors if no timer is running."
    )]
    async fn solidtime_stop_timer(
        &self,
        Parameters(_): Parameters<EmptyParams>,
    ) -> Result<CallToolResult, McpError> {
        let Some(entry) = self.client.active().await.map_err(err)? else {
            return Err(err("no timer is running"));
        };
        let id = entry
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| err("active entry has no id"))?;
        let now = chrono::Utc::now();
        let updated = self.client.set_end(id, &t::fmt_utc(now)).await.map_err(err)?;
        ok(json!({ "stopped": true, "id": id, "end_local": t::fmt_local_hm(now), "entry": updated }))
    }

    #[tool(
        description = "Solidtime: create a completed time entry from Oslo-local date + start + end (HH:MM). Converts to UTC automatically. Provide project_id and description when known."
    )]
    async fn solidtime_create_entry(
        &self,
        Parameters(p): Parameters<CreateEntryParams>,
    ) -> Result<CallToolResult, McpError> {
        let start = t::local_to_utc(&p.date, &p.start).map_err(err)?;
        let end = t::local_to_utc(&p.date, &p.end).map_err(err)?;
        if end <= start {
            return Err(err("end must be after start (same-day block)"));
        }
        let entry = self
            .client
            .create_entry(
                p.project_id.as_deref().unwrap_or(""),
                &t::fmt_utc(start),
                Some(&t::fmt_utc(end)),
                p.description.as_deref().unwrap_or(""),
                p.billable.unwrap_or(true),
            )
            .await
            .map_err(err)?;
        ok(json!({
            "created": true,
            "date": p.date,
            "start_local": p.start,
            "end_local": p.end,
            "hours": t::hours((end - start).num_seconds()),
            "entry": entry,
        }))
    }

    #[tool(
        description = "Solidtime: summarise the Oslo-local week (Monday–Sunday) containing the given date (defaults to today): per-day and per-project totals plus the week total."
    )]
    async fn solidtime_week_summary(
        &self,
        Parameters(p): Parameters<DateParams>,
    ) -> Result<CallToolResult, McpError> {
        let date = self.resolve_date(p.date.as_deref())?;
        let (monday, sunday) = t::week_bounds(date);
        let result = entries_between(&self.client, monday, sunday).await?;
        let mut by_project: BTreeMap<String, i64> = BTreeMap::new();
        if let Some(entries) = result.get("entries").and_then(Value::as_array) {
            for entry in entries {
                let name = entry
                    .get("project")
                    .and_then(Value::as_str)
                    .unwrap_or("(unknown)")
                    .to_string();
                *by_project.entry(name).or_default() +=
                    entry.get("duration_seconds").and_then(Value::as_i64).unwrap_or(0);
            }
        }
        let projects: Vec<Value> = by_project
            .iter()
            .map(|(name, seconds)| json!({ "project": name, "hours": t::hours(*seconds) }))
            .collect();
        ok(json!({
            "week_start": monday.to_string(),
            "week_end": sunday.to_string(),
            "total_hours": result["total_hours"].clone(),
            "by_day": result["daily"].clone(),
            "by_project": projects,
        }))
    }

    #[tool(
        description = "Activity: recent GitLab events (read-only, via the authenticated glab CLI), filtered to one Oslo-local day (defaults to today), with project paths resolved."
    )]
    async fn activity_gitlab(
        &self,
        Parameters(p): Parameters<GitlabParams>,
    ) -> Result<CallToolResult, McpError> {
        let date = self.resolve_date(p.date.as_deref())?;
        let host = self.host(p.host.as_deref());
        let events = events_for_day(date, host, p.per_page.unwrap_or(100)).await?;
        ok(json!({ "date": date.to_string(), "host": host, "count": events.len(), "events": events }))
    }

    #[tool(
        description = "Activity: list Multica issues (read-only, via the authenticated multica CLI). Returns the raw list payload."
    )]
    async fn activity_multica(
        &self,
        Parameters(p): Parameters<MulticaParams>,
    ) -> Result<CallToolResult, McpError> {
        let workspace = p.workspace_id.unwrap_or_default();
        let value = activity::multica_issues(&workspace, p.limit.unwrap_or(50))
            .await
            .map_err(err)?;
        ok(value)
    }

    #[tool(
        description = "Glue: for one Oslo-local day, show logged Solidtime entries next to that day's GitLab activity, and flag GitLab events whose title/project does not appear in any logged description — i.e. work that may not be logged yet."
    )]
    async fn worklog_check_gaps(
        &self,
        Parameters(p): Parameters<GitlabParams>,
    ) -> Result<CallToolResult, McpError> {
        let date = self.resolve_date(p.date.as_deref())?;
        let host = self.host(p.host.as_deref());
        let logged = entries_between(&self.client, date, date).await?;
        let events = events_for_day(date, host, p.per_page.unwrap_or(100)).await?;

        let haystack = logged
            .get("entries")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|e| e.get("description").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n")
                    .to_lowercase()
            })
            .unwrap_or_default();

        let unlogged: Vec<Value> = events
            .iter()
            .filter(|event| {
                let title = event.get("title").and_then(Value::as_str).unwrap_or("");
                let project = event.get("project").and_then(Value::as_str).unwrap_or("");
                !title.is_empty()
                    && !haystack.contains(&title.to_lowercase())
                    && (project.is_empty() || !haystack.contains(&project.to_lowercase()))
            })
            .cloned()
            .collect();

        ok(json!({
            "date": date.to_string(),
            "logged_total_hours": logged["total_hours"].clone(),
            "logged_entries": logged["entries"].clone(),
            "gitlab_event_count": events.len(),
            "possibly_unlogged": unlogged,
        }))
    }

    #[tool(
        description = "Glue: suggest candidate work blocks for one Oslo-local day by grouping that day's GitLab activity by project. Does NOT invent exact start/end times — it reports each project's activity time span and titles so a human or agent can decide the real block."
    )]
    async fn worklog_suggest_entries(
        &self,
        Parameters(p): Parameters<GitlabParams>,
    ) -> Result<CallToolResult, McpError> {
        let date = self.resolve_date(p.date.as_deref())?;
        let host = self.host(p.host.as_deref());
        let events = events_for_day(date, host, p.per_page.unwrap_or(100)).await?;

        let mut groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for event in events {
            let project = event
                .get("project")
                .and_then(Value::as_str)
                .unwrap_or("(unknown)")
                .to_string();
            groups.entry(project).or_default().push(event);
        }

        let suggestions: Vec<Value> = groups
            .iter()
            .map(|(project, items)| {
                let times: Vec<&str> = items
                    .iter()
                    .filter_map(|e| e.get("time_local").and_then(Value::as_str))
                    .collect();
                let first = times.first().copied().unwrap_or("");
                let last = times.last().copied().unwrap_or("");
                let titles: Vec<String> = items
                    .iter()
                    .filter_map(|e| e.get("title").and_then(Value::as_str))
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                json!({
                    "project": project,
                    "activity_first_local": first,
                    "activity_last_local": last,
                    "event_count": items.len(),
                    "titles": titles,
                    "note": "Activity span is NOT a worked block; confirm real start/end before logging.",
                })
            })
            .collect();

        ok(json!({ "date": date.to_string(), "suggestions": suggestions }))
    }
}
