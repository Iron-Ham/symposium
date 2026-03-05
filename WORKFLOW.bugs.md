---
tracker:
  kind: notion
  mcp_url: "https://mcp-dev.notion.com/readonly"
  database_id: "1cfb35e6-e67f-8168-bc1e-000b75bfd45a"
  active_states: ["On Deck", "In Progress", "Backlog"]
  terminal_states: ["Fixed", "Won't Fix", "Can't Fix", "No Longer Relevant", "Expected Behavior"]
  property_id: "userDefined:ID"
  property_title: "Task Name"
  property_status: "Status"
  property_priority: "Bug Priority"
  property_description: "Notes"

polling:
  interval_ms: 30000

workspace:
  root: "~/symposium_workspaces/mail-bugs"

hooks:
  after_create: |
    git clone git@github.com:makenotion/mail.git .
    git checkout -b symposium/bug-{{ issue.identifier }}
  before_run: |
    git fetch origin main
    git rebase origin/main

agent:
  max_concurrent_agents: 3
  max_turns: 20

codex:
  command: "claude-code app-server"
  turn_timeout_ms: 3600000
  stall_timeout_ms: 300000

server:
  port: 8081
---
You are working on bug {{ issue.identifier }}: {{ issue.title }}.

Platform: {{ issue.platform }}
Severity: {{ issue.priority }}

{{ issue.description }}

You are working in the Notion Mail monorepo. Before starting, read `CLAUDE.md` at the repo root and the relevant subsystem `AGENTS.md` for the area you're working in.

Key subsystem guides:
- Mail backend: `services/mail/AGENTS.md`
- Mail frontend: `mail-web/AGENTS.md`
- iOS client: `mail-ios/AGENTS.md`

This is a bug fix. Focus on:
1. Reproducing the issue (read the bug description carefully)
2. Finding the root cause
3. Implementing the minimal fix
4. Writing or updating tests to cover the regression

Always build shared libraries first with `yarn build:lib` before building or running services.

{% if attempt %}
This is retry attempt {{ attempt }}. Review what happened in the previous attempt and continue from where you left off.
{% endif %}
