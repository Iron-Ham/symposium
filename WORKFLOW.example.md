# Symposium Workflow Configuration

Copy this file to `WORKFLOW.<name>.md` and fill in your values.
Files matching `WORKFLOW.*.md` (except this example) are gitignored.

```yaml
tracker:
  kind: notion
  # Option A: stdio MCP server (default)
  mcp_command: "npx -y @notionhq/notion-mcp-server"
  # Option B: HTTP MCP server with OAuth
  # mcp_url: "https://your-notion-mcp-server.example.com"
  database_id: "your-notion-database-id"
  active_states:
    - "Todo"
    - "In Progress"
  terminal_states:
    - "Done"
    - "Cancelled"
  property_id: "ID"
  property_title: "Name"
  property_status: "Status"
  property_priority: "Priority"
  property_description: "Description"

polling:
  interval_ms: 30000

workspace:
  root: "~/symposium_workspaces"

hooks:
  after_create: |
    cd {{ workspace }}
    git clone {{ repo_url }} . 2>/dev/null || true
    git checkout -b symposium/{{ issue.identifier | downcase }}
  after_run: |
    cd {{ workspace }}
    git add -A
    git commit -m "{{ issue.identifier }}: {{ issue.title }}" --allow-empty

agent:
  max_concurrent_agents: 5
  max_turns: 20

codex:
  command: "claude-code app-server"
  turn_timeout_ms: 3600000
  stall_timeout_ms: 300000

server:
  port: 8080
```

## Prompt Template

Below is the Liquid template sent to the coding agent for each issue.

```
You are working on issue {{ issue.identifier }}: {{ issue.title }}.

Status: {{ issue.status }}
{% if issue.priority %}Priority: {{ issue.priority }}{% endif %}
{% if issue.description %}
## Description
{{ issue.description }}
{% endif %}

Work in the directory: {{ workspace }}
```
