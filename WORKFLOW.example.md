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
    git -C ~/Developer/my-org/my-repo worktree add {{ workspace }} -b symposium/bug-{{ issue.safe_identifier }}
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

# Optional: inject supplementary MCP servers into agent sessions.
# These are passed to the claude CLI via --mcp-config.
# Useful for giving agents access to Sentry, Datadog, PagerDuty, etc.
# mcp_servers:
#   sentry:
#     type: http
#     url: "https://mcp.sentry.dev/mcp"
#   custom-linter:
#     type: stdio
#     command: "npx"
#     args: ["-y", "@my-org/linter-mcp"]
#     env:
#       API_KEY: "$MY_API_KEY"  # env vars are expanded

# Optional: poll Sentry for crashes alongside Notion issues.
# Both sources are merged into a single dispatch queue each tick.
# Sentry issue IDs are prefixed with "sentry:" to avoid collisions.
# sentry:
#   enabled: true
#   org: "my-org"
#   project: "my-project"
#   mcp_url: "https://mcp.sentry.dev/mcp"  # Sentry MCP server (OAuth auth)
#   query: "release:[my-app@1.7.*,my-app@1.8.*]"  # Sentry search syntax
#   min_events: 5                           # skip issues below this threshold

# Optional: run a pre-flight verification step before the main agent.
# When enabled, a separate agent session runs first to verify the issue is
# still valid (e.g. reproduce the bug, check if already fixed). If the agent
# writes a PREFLIGHT_SKIP file, the issue is skipped entirely — no
# implementation, review, or PR is created.
# preflight:
#   enabled: true
#   prompt_template: |
#     You are verifying bug {{ issue.identifier }}: {{ issue.title }}.
#     {{ issue.description }}
#     Walk through the code and confirm this bug is real and reproducible.
#     If the bug involves UI behavior, build a preview and test it.

# Optional: configure the post-completion review step
review:
  # Set to false to skip the review step entirely (default: true)
  # enabled: false
  # Custom Liquid template for the review prompt (uses built-in default if empty).
  # Same variables as the main prompt template: {{ issue.identifier }}, {{ issue.title }}, etc.
  # prompt_template: |
  #   Review changes for {{ issue.identifier }}: {{ issue.title }}.
  #   Run `/deep-review --changes` and fix any issues found.
  #   Commit fixes with `git add` and `git commit`.
  # Optional: shell hook to run before the review agent starts
  # (e.g. generate a lint report the agent can read)
  # before_review: |
  #   cd {{ workspace }} && npx eslint --format json -o review-report.json src/

# Optional: monitor open PRs for reviewer feedback and auto-dispatch fix agents.
# When enabled, each tick checks PRs created by Symposium for new comments or
# change requests. If actionable feedback is found, an agent is spun up in the
# existing workspace to address it.
# pr_review:
#   enabled: true
#   # Which reviewers trigger a fix agent: "all", "humans", or a list of usernames.
#   # "humans" skips GitHub bot accounts (usernames ending in [bot]).
#   # Default: "all"
#   reviewers: humans
#   # reviewers: ["alice", "bob"]   # only react to specific reviewers
#   # Custom Liquid template for the PR review prompt.
#   # Available variables: {{ issue.identifier }}, {{ issue.title }}, {{ issue.extra.pr_number }}
#   # If empty, uses a built-in default that reads comments via `gh pr view`.
#   # prompt_template: |
#   #   Address reviewer feedback on PR #{{ issue.extra.pr_number }} for {{ issue.identifier }}.
#   #   Run `gh pr view {{ issue.extra.pr_number }} --comments` to see feedback.
---
```

## Prompt Template

Everything after the YAML front matter closing `---` is a Liquid template.
It is rendered per-issue and sent to the agent as its initial prompt via stdin.

Available variables:
- `{{ issue.identifier }}` — issue ID (e.g. "316205" or "sentry:MAIL-IOS-3B")
- `{{ issue.safe_identifier }}` — branch-safe version of the ID (colons → hyphens)
- `{{ issue.title }}` — issue title
- `{{ issue.description }}` — issue description/notes
- `{{ issue.status }}` — current status
- `{{ issue.priority }}` — priority level
- `{{ issue.url }}` — link to the issue page
- `{{ issue.comments }}` — formatted page comments (author + timestamp + body)
- `{{ issue.<property> }}` — any extra Notion property (lowercased column name)
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

## Pre-flight Verification

When `preflight.enabled` is set to `true`, Symposium runs a separate agent session
**before** the main implementer to verify the issue is still valid. This is especially
useful for bug workflows where issues may become stale, get fixed by other changes, or
turn out to be expected behavior.

The preflight agent runs with full access to the workspace, MCP servers, and tools —
just like the main agent. Its prompt is a Liquid template with all the same variables
(`{{ issue.identifier }}`, `{{ issue.title }}`, `{{ issue.description }}`, etc.).

### Skip signal

If the preflight agent determines the issue should be skipped, it writes a
`PREFLIGHT_SKIP` file in its working directory containing a brief explanation.
Symposium reads this file and short-circuits: no implementation, no review, no PR.
The issue is marked as handled and will not be retried.

If the file is not written, execution proceeds to the main agent automatically.

### Fail-open design

If the preflight agent fails to start or errors during execution, Symposium logs a
warning and proceeds to the main agent. A broken preflight never blocks real work.
Similarly, if the `prompt_template` has a Liquid syntax error, the preflight is
skipped with a warning.

### Example: bug verification

```yaml
preflight:
  enabled: true
  prompt_template: |
    You are verifying bug {{ issue.identifier }}: {{ issue.title }}.
    {{ issue.description }}
    Walk through the relevant code paths and confirm the bug exists.
    If you can reproduce it via tests or a build, do so.
```

## PR Metadata

Symposium automatically instructs agents to write PR metadata files in the workspace root:

- **`PR_TITLE`** — A single line with the PR title. Should be a concise, human-readable
  summary of the actual change (not just the bug title). No conventional commit prefixes.
- **`PR_BODY.md`** — Markdown PR body including investigation reasoning, what was changed
  and why, and a link back to the issue (e.g. `Fixes 316205`).

The **implementer agent** writes these files after committing its fix, since it has the
full context of what it investigated and why it chose its approach. The **review agent**
then updates them if it made any additional changes.

If the files are missing, Symposium falls back to a generic title/body based on the
issue ID and title.

These files are **not** committed to git — they are read from disk and then cleaned up.

## PR Review Monitoring

When `pr_review.enabled` is set to `true`, Symposium monitors open PRs it created for
reviewer feedback. Each tick, it runs `gh pr view` to check for new comments or change
requests. If actionable feedback is detected, it spins up an agent in the **existing
workspace** to address the feedback — no new workspace or branch is created.

Reviews are grouped per-author, and only the latest review state per author is considered.
This means if a reviewer requests changes and later approves, the approval supersedes the
earlier request and no fix agent is triggered.

The `reviewers` filter controls which reviewers' feedback triggers a response:

| Value | Behavior |
|-------|----------|
| `"all"` (default) | React to any reviewer |
| `"humans"` | Skip bot accounts (GitHub usernames ending in `[bot]`) |
| `["alice", "bob"]` | Only react to these specific GitHub usernames |
