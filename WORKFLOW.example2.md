# Symposium Workflow: Tasks Example

This example shows a workflow for general tasks (features, enhancements, tech debt)
rather than bug fixes. It demonstrates different config choices compared to the
bug-focused `WORKFLOW.example.md`.

Run alongside a bug workflow:
```sh
symposium WORKFLOW.bugs.md WORKFLOW.tasks.md --max-agents 5
```

---

```yaml
---
tracker:
  kind: notion
  mcp_command: "npx -y @notionhq/notion-mcp-server"
  database_id: "your-tasks-database-uuid"
  # Pick up tasks that are ready to work on
  active_states: ["Ready to Build", "To Do", "On Deck"]
  terminal_states: ["Completed", "Cancelled", "Won't Fix", "Archived", "Not Done"]
  property_id: "userDefined:ID"
  property_title: "Title"
  property_status: "Status"
  property_priority: "Priority"
  property_description: "Description"
  id_prefix: "TASK-"
  property_assignee: "Assignee"
  assignee_user_id: "your-notion-user-uuid"
  # Skip tasks that already have a linked PR
  skip_if_set: "GitHub Pull Requests"

polling:
  interval_ms: 60000  # Tasks are less urgent -- poll every 60s

workspace:
  root: "~/symposium_workspaces/my-project-tasks"

hooks:
  after_create: |
    set -e
    repo="$HOME/Developer/my-org/my-repo"
    branch="symposium/task-{{ issue.safe_identifier }}"
    workdir="{{ workspace }}"
    [ -d "$repo/.git" ] || { echo "after_create: $repo is not a git repo" >&2; exit 2; }
    # Self-healing: evict any worktree/branch left over from a prior run.
    existing=$(git -C "$repo" worktree list --porcelain | awk -v b="refs/heads/$branch" '/^worktree /{w=$2} $1=="branch" && $2==b {print w; exit}')
    if [ -n "$existing" ]; then
      git -C "$repo" worktree unlock "$existing" 2>/dev/null || true
      git -C "$repo" worktree remove -f -f "$existing" 2>/dev/null || true
    fi
    git -C "$repo" branch -D "$branch" 2>/dev/null || true
    rm -rf "$workdir"
    git -C "$repo" worktree prune
    git -C "$repo" worktree add "$workdir" -b "$branch"
  before_remove: |
    repo="$HOME/Developer/my-org/my-repo"
    git -C "$repo" worktree remove --force {{ workspace }} 2>/dev/null || true
    git -C "$repo" branch -D symposium/task-{{ issue.safe_identifier }} 2>/dev/null || true

agent:
  max_concurrent_agents: 2  # Leave headroom for bug workflows

codex:
  # Path to the Claude CLI binary (default: "claude-code app-server")
  command: "/usr/local/bin/claude"
  turn_timeout_ms: 3600000
  stall_timeout_ms: 300000

server:
  port: 8080  # Shared across workflows when run together

review:
  enabled: true

pr_review:
  enabled: true
  reviewers: humans
---
```

## Prompt Template

```liquid
You are working on task {{ issue.identifier }}: {{ issue.title }}.

{% if issue.priority %}Priority: {{ issue.priority }}{% endif %}

{{ issue.description }}

Before starting, read `CLAUDE.md` at the repo root for project conventions.

This is a feature/enhancement task. Focus on:
1. First, rebase on the latest main: `git fetch origin main && git rebase origin/main`
2. Understanding the requirements from the task description
3. Implementing the change with clean, well-structured code
4. Writing tests for the new functionality
5. Commit your changes with `git add` and `git commit` using a descriptive message

{% if issue.comments != blank %}
Discussion and context from the team:
{{ issue.comments }}
{% endif %}

{% if attempt %}
This is retry attempt {{ attempt }}. Review what happened in the previous attempt and continue from where you left off.
{% endif %}
```
