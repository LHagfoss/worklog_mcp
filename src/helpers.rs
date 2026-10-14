//! Shared shaping helpers: turn raw Solidtime/GitLab payloads into compact,
//! Oslo-local JSON that is easy for an agent to read.

use std::collections::BTreeMap;

use rmcp::ErrorData as McpError;
use serde_json::{Value, json};

use crate::activity;
use crate::err;
use crate::solidtime::Solidtime;
use crate::timeutil as t;

/// Extract the `.data` array from a Solidtime list response.
pub fn data_array(value: &Value) -> Vec<Value> {
    value
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Map project id -> (name, client_id) from a projects response.
pub fn project_index(projects: &Value) -> BTreeMap<String, (String, String)> {
    let mut map = BTreeMap::new();
    for project in data_array(projects) {
        let id = project
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let name = project
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("(unnamed)")
            .to_string();
        let client = project
            .get("client_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        map.insert(id.to_string(), (name, client));
    }
    map
}

/// Normalize one raw time entry into a compact, Oslo-local shape.
pub fn shape_entry(entry: &Value, projects: &BTreeMap<String, (String, String)>) -> Value {
    let start = entry
        .get("start")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let end = entry.get("end").and_then(Value::as_str);
    let duration = entry.get("duration").and_then(Value::as_i64).unwrap_or(0);
    let project_id = entry
        .get("project_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let project_name = projects
        .get(project_id)
        .map(|(name, _)| name.clone())
        .unwrap_or_else(|| {
            if project_id.is_empty() {
                "No project".into()
            } else {
                project_id.to_string()
            }
        });

    let (date, local_start, local_end, running) = match t::parse_utc(start) {
        Ok(s) => {
            let s_local = t::fmt_local_hm(s);
            let date = s.with_timezone(&t::OSLO).date_naive().to_string();
            match end.and_then(|e| t::parse_utc(e).ok()) {
                Some(e) => (date, s_local, t::fmt_local_hm(e), false),
                None => (date, s_local, "(running)".into(), true),
            }
        }
        Err(_) => (String::new(), "(invalid)".into(), "(invalid)".into(), false),
    };

    json!({
        "id": entry.get("id").cloned().unwrap_or(Value::Null),
        "date": date,
        "start_local": local_start,
        "end_local": local_end,
        "running": running,
        "hours": t::hours(duration),
        "duration_seconds": duration,
        "project": project_name,
        "project_id": if project_id.is_empty() { Value::Null } else { json!(project_id) },
        "description": entry.get("description").cloned().unwrap_or(Value::Null),
        "billable": entry.get("billable").cloned().unwrap_or(json!(false)),
        "start_utc": start,
        "end_utc": match end { Some(e) => json!(e), None => Value::Null },
    })
}

/// Fetch + shape all entries between two local dates (inclusive), plus totals.
pub async fn entries_between(
    client: &Solidtime,
    from: chrono::NaiveDate,
    to: chrono::NaiveDate,
) -> Result<Value, McpError> {
    let projects = project_index(&client.projects().await.map_err(err)?);
    let start_utc = t::fmt_utc(t::day_start_utc(from));
    let end_utc = t::fmt_utc(t::day_end_utc(to));
    let raw = client
        .time_entries(&start_utc, &end_utc)
        .await
        .map_err(err)?;

    let mut entries: Vec<Value> = data_array(&raw)
        .iter()
        .map(|e| shape_entry(e, &projects))
        .collect();
    entries.sort_by(|a, b| {
        a["start_utc"]
            .as_str()
            .unwrap_or("")
            .cmp(b["start_utc"].as_str().unwrap_or(""))
    });

    let total_seconds: i64 = entries
        .iter()
        .map(|e| e["duration_seconds"].as_i64().unwrap_or(0))
        .sum();

    let mut by_day: BTreeMap<String, i64> = BTreeMap::new();
    for entry in &entries {
        if let Some(date) = entry["date"].as_str() {
            *by_day.entry(date.to_string()).or_default() +=
                entry["duration_seconds"].as_i64().unwrap_or(0);
        }
    }
    let daily: Vec<Value> = by_day
        .iter()
        .map(|(date, seconds)| json!({ "date": date, "hours": t::hours(*seconds) }))
        .collect();

    Ok(json!({
        "from": from.to_string(),
        "to": to.to_string(),
        "total_hours": t::hours(total_seconds),
        "daily": daily,
        "entries": entries,
    }))
}

/// GitLab events for a single Oslo-local date, enriched with project path.
pub async fn events_for_day(
    date: chrono::NaiveDate,
    host: &str,
    per_page: u32,
) -> Result<Vec<Value>, McpError> {
    let events = activity::gitlab_events(host, per_page).await.map_err(err)?;
    let project_map = activity::gitlab_project_map(host).await.unwrap_or_default();
    let mut out = Vec::new();
    for event in events {
        let Some(created) = event.get("created_at").and_then(Value::as_str) else {
            continue;
        };
        let Ok(dt) = t::parse_utc(created) else {
            continue;
        };
        if dt.with_timezone(&t::OSLO).date_naive() != date {
            continue;
        }
        let project_id = event.get("project_id").and_then(Value::as_i64);
        let project = event
            .get("project_path")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                project_id
                    .map(|id| id.to_string())
                    .and_then(|id| project_map.get(&id).cloned())
            })
            .unwrap_or_default();
        let title = event
            .get("target_title")
            .and_then(Value::as_str)
            .or_else(|| {
                event
                    .get("note")
                    .and_then(|n| n.get("note"))
                    .and_then(Value::as_str)
            })
            .unwrap_or("")
            .to_string();
        out.push(json!({
            "time_local": t::fmt_local_hm(dt),
            "action": event.get("action_name").cloned().unwrap_or(Value::Null),
            "project": project,
            "title": title,
        }));
    }
    Ok(out)
}
