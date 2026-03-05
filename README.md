# Symposium

A Rust implementation of the [OpenAI Symphony spec](https://github.com/openai/symphony/blob/main/SPEC.md) — a long-running orchestration service that polls an issue tracker, creates isolated per-issue workspaces, and runs coding agent sessions.

## Key Differences from the Spec

1. **Notion as tracker** — issues live in a Notion database (queried via MCP server) instead of Linear
2. **MCP proxy architecture** — communicates with Notion through a spawned MCP server process (JSON-RPC over stdio) rather than calling the REST API directly

## Architecture

```
CLI (clap)
  └─ Orchestrator (tokio event loop)
       ├─ Config Layer (WORKFLOW.md watcher + typed config)
       ├─ Notion Tracker (MCP client → notion MCP server)
       ├─ Workspace Manager (fs + hook subprocess)
       ├─ Agent Runner (app-server subprocess, JSON-RPC stdio)
       ├─ HTTP Server (axum: dashboard + /api/v1/*)
       └─ Structured Logging (tracing)
```

## Getting Started

### Prerequisites

- Rust 1.85+ (edition 2024)
- A Notion database with `ID`, `Status`, and `Priority` properties
- A Notion MCP server (e.g. `npx -y @notionhq/notion-mcp-server`)
- A coding agent that speaks the app-server JSON-RPC protocol

### Build

```sh
cargo build --release
```

### Configure

Create a `WORKFLOW.md` file with YAML front matter and a prompt template:

```yaml
---
tracker:
  kind: notion
  mcp_command: "npx -y @notionhq/notion-mcp-server"
  database_id: "your-database-id"
  active_states: ["Todo", "In Progress"]
  terminal_states: ["Done", "Cancelled"]
  property_id: "ID"
  property_status: "Status"
  property_priority: "Priority"

polling:
  interval_ms: 30000

workspace:
  root: "~/symposium_workspaces"

hooks:
  after_create: |
    git clone https://github.com/your/repo.git .
  before_run: |
    git pull --rebase origin main

agent:
  max_concurrent_agents: 5
  max_turns: 20

codex:
  command: "claude-code app-server"
  turn_timeout_ms: 3600000
  stall_timeout_ms: 300000

server:
  port: 8080
---
You are working on issue {{ issue.identifier }}: {{ issue.title }}.

{{ issue.description }}

{% if attempt %}
This is retry attempt {{ attempt }}. Review what happened in the previous attempt and continue.
{% endif %}
```

### Run

```sh
symposium WORKFLOW.md --port 8080
```

The orchestrator will:
1. Parse `WORKFLOW.md` for config and prompt template
2. Start an HTTP dashboard at `http://localhost:8080`
3. Poll Notion for issues in active states
4. For each eligible issue: create a workspace, run hooks, start an agent session
5. Retry failed issues with exponential backoff
6. Clean up workspaces when issues reach terminal states

### HTTP API

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/` | GET | HTML dashboard with auto-refresh |
| `/api/v1/state` | GET | Full orchestrator state as JSON |
| `/api/v1/issues/{id}` | GET | Detail for a single running issue |
| `/api/v1/refresh` | POST | Trigger an immediate poll cycle |

## Config Hot-Reload

Edit `WORKFLOW.md` while the service is running — changes are picked up automatically (500ms debounce). Parse errors are logged and the previous config is retained.

## License

[MIT](LICENSE)
