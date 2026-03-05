# Symposium Workflow Configuration

Copy this file to `WORKFLOW.<name>.md` and fill in your values.
Files matching `WORKFLOW.*.md` (except this example) are gitignored.

---

The file uses YAML front matter for configuration, followed by a Liquid
template that becomes the prompt sent to each agent.

## Configuration

```yaml
---
tracker:
  kind: notion
  # Option A: stdio MCP server (public npm package)
  mcp_command: "npx -y @notionhq/notion-mcp-server"
  # Option B: HTTP MCP server (e.g. Notion internal dev endpoint)
  # mcp_url: "https://mcp.notion.com/readonly"
  database_id: "your-database-uuid"
  active_states: ["On Deck", "In Progress", "Backlog"]
  terminal_states: ["Fixed", "Won't Fix", "Can't Fix", "No Longer Relevant"]
  property_id: "userDefined:ID"
  property_title: "Task Name"
  property_status: "Status"
  property_priority: "Priority"
  property_description: "Description"
  # Optional: only pick up issues assigned to this Notion user ID
  # assignee_user_id: "your-notion-user-uuid"
  # Optional: skip issues where this relation property is non-null (e.g. linked PRs)
  # skip_if_set: "GitHub Pull Requests"

polling:
  interval_ms: 30000

workspace:
  root: "~/symposium_workspaces/my-project"
  # Optional: run the agent in a subdirectory of the workspace
  # agent_subdirectory: "packages/frontend"

hooks:
  # Runs once when a new workspace is created for an issue.
  # Use git worktree for fast, lightweight checkouts from a local repo:
  after_create: |
    git -C ~/Developer/my-org/my-repo worktree add {{ workspace }} -b symposium/bug-{{ issue.identifier }}
  # Optional: runs before each agent attempt (retries included)
  # before_run: |
  #   git fetch origin main && git rebase origin/main
  # Optional: runs after each agent attempt
  # after_run: |
  #   echo "Agent finished with RUN_SUCCESS=$RUN_SUCCESS"

agent:
  max_concurrent_agents: 3

codex:
  command: "/usr/local/bin/claude"
  turn_timeout_ms: 3600000    # 1 hour max per agent session
  stall_timeout_ms: 300000    # 5 min with no activity → stalled

server:
  port: 8080
---
```

## Prompt Template

Everything after the YAML front matter closing `---` is a Liquid template.
It is rendered per-issue and sent to the agent as its initial prompt via stdin.

Available variables:
- `{{ issue.identifier }}` — issue ID (e.g. "316205")
- `{{ issue.title }}` — issue title
- `{{ issue.description }}` — issue description/notes
- `{{ issue.status }}` — current status
- `{{ issue.priority }}` — priority level
- `{{ attempt }}` — retry attempt number (nil on first run)

```liquid
You are working on bug {{ issue.identifier }}: {{ issue.title }}.

{% if issue.priority %}Severity: {{ issue.priority }}{% endif %}

{{ issue.description }}

Before starting, read `CLAUDE.md` at the repo root for project conventions.

This is a bug fix. Focus on:
1. First, rebase on the latest main: `git fetch origin main && git rebase origin/main`
2. Reproducing the issue (read the bug description carefully)
3. Finding the root cause
4. Implementing the minimal fix
5. Writing or updating tests to cover the regression
6. Commit your changes with `git add` and `git commit` using a descriptive message

{% if attempt %}
This is retry attempt {{ attempt }}. Review what happened in the previous attempt and continue from where you left off.
{% endif %}
```
