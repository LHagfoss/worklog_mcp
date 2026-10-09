# worklog_mcp

A local stdio [MCP](https://modelcontextprotocol.io) server for time logging.

It exposes **typed tools** over the [Solidtime](https://solidtime.io) REST API
(Solidtime has no official MCP), plus read-only GitLab/Multica activity signals
and glue tools that help reconcile logged hours with real work.

Written in Rust with [`rmcp`](https://crates.io/crates/rmcp), matching the
pattern of the other local `*_mcp` servers.

## Tools

### Solidtime

| Tool | Description |
| --- | --- |
| `solidtime_whoami` | Authenticated user + organization/member IDs (connectivity check). |
| `solidtime_list_projects` | Projects with id, name, client_id, archived flag. |
| `solidtime_list_clients` | Clients with id, name, archived flag. |
| `solidtime_list_entries` | Time entries for one Oslo-local day (defaults to today). |
| `solidtime_list_entries_range` | Time entries across an inclusive Oslo-local date range. |
| `solidtime_get_active_timer` | The currently running entry, if any. |
| `solidtime_start_timer` | Start a running timer now (fails if one is already running). |
| `solidtime_stop_timer` | Stop the running timer by setting its end to now. |
| `solidtime_create_entry` | Create a completed entry from a local date + start/end times. |
| `solidtime_week_summary` | Per-day and per-project totals for the week containing a date. |

### Activity (read-only)

| Tool | Description |
| --- | --- |
| `activity_gitlab` | Recent GitLab events for one day, via the authenticated `glab` CLI. |
| `activity_multica` | Multica issues, via the authenticated `multica` CLI. |

### Glue

| Tool | Description |
| --- | --- |
| `worklog_check_gaps` | Logged entries vs. that day's GitLab activity; flags work that may not be logged. |
| `worklog_suggest_entries` | Groups a day's GitLab activity by project as candidate blocks (never invents times). |

## Time handling

All input and output times are **Europe/Oslo local**. The Solidtime API requires
UTC ISO-8601, and every conversion goes through `src/timeutil.rs` — offsets are
never hand-computed. Daylight-saving transitions are handled.

## Configuration

### Environment

| Variable | Required | Purpose |
| --- | --- | --- |
| `SOLIDTIME_API_KEY` | yes* | Solidtime API token. |
| `SOLIDTIME_TOKEN` / `TOKEN` | fallback | Alternative token names. |
| `SOLIDTIME_BASE_URL` | no | Defaults to `https://solidtime.paral.no`. |
| `WORKLOG_ENV_FILE` | no | Path to a `.env` file to load when the token is not already in the shell environment. |
| `MULTICA_WORKSPACE_ID` | no | Workspace for `activity_multica`; defaults to the CLI's configured workspace. |

\* A token must be available as a shell variable, or via the file named by
`WORKLOG_ENV_FILE`. The server exits with a clear message if neither is set.

Activity tools shell out to `glab` and `multica`; make sure both are installed
and authenticated for the relevant hosts.

### Registering with RustCode

Add to `~/.config/rustcode/config.toml`:

```toml
[[mcp_servers]]
name = "worklog"
command = "/Users/lagos/code/worklog_mcp/target/release/worklog_mcp"
args = []
enabled = true
always_include = false

[mcp_servers.env]
SOLIDTIME_BASE_URL = "https://solidtime.paral.no"
WORKLOG_ENV_FILE = "/path/to/.env"   # a file containing TOKEN=...
```

## Build

```sh
cargo build --release
cargo test
```

The binary is written to `target/release/worklog_mcp` and speaks MCP over stdio.

## Layout

```
src/
  main.rs       # entrypoint, env loading, server bootstrap
  tools.rs      # MCP tool definitions (@tool_router)
  solidtime.rs  # Solidtime REST client
  activity.rs   # glab / multica read-only wrappers
  helpers.rs    # payload shaping into Oslo-local JSON
  timeutil.rs   # Europe/Oslo <-> UTC conversions
```
