# Symposium Workflow Configuration

Copy this file to `WORKFLOW.<name>.md` and fill in your values.
Files matching `WORKFLOW.*.md` (except examples) are gitignored.

## Multi-Workflow Support

Symposium supports running multiple workflows simultaneously from a single process.
Each workflow file gets its own tracker, config, polling interval, and workspace root.

```sh
# Single workflow (backward compatible)
symposium WORKFLOW.md

# Multiple workflows
symposium WORKFLOW.bugs.md WORKFLOW.sentry.md WORKFLOW.tasks.md

# With a global agent cap across all workflows
symposium WORKFLOW.bugs.md WORKFLOW.tasks.md --max-agents 4

# Override the dashboard port (default: from first workflow's server.port, or 8080)
symposium WORKFLOW.bugs.md --port 9090

# JSON-formatted logs (useful for log aggregation)
symposium WORKFLOW.bugs.md --json-logs
```

The workflow ID is derived from the filename: `WORKFLOW.bugs.md` becomes `"bugs"`,
`WORKFLOW.md` becomes `"default"`. Each workflow's issues are namespaced in state
so the same issue ID in two different workflows won't collide.

Concurrency is enforced per-workflow (each workflow's `max_concurrent_agents`) plus
an optional global cap (`--max-agents`). Without a global cap, workflows run
independently.

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
  property_assignee: "Assignee"
  # Optional: only pick up issues assigned to this Notion user ID
  # assignee_user_id: "your-notion-user-uuid"
  # Optional: skip issues where this relation property is non-null (e.g. linked PRs)
  # skip_if_set: "GitHub Pull Requests"
  # Optional: prefix prepended to the raw ID value (e.g. "BUG-" -> "BUG-316205")
  # Affects issue identifiers everywhere: state keys, branch names, PR titles.
  # id_prefix: "BUG-"

polling:
  interval_ms: 30000

workspace:
  root: "~/symposium_workspaces/my-project"
  # Optional: run the agent in a subdirectory of the workspace
  # agent_subdirectory: "packages/frontend"

hooks:
  # Runs once when a new workspace is created for an issue.
  # Self-healing: if a prior run left an orphan branch or worktree (e.g. the
  # before_remove hook didn't run cleanly), evict it before creating the new
  # one. Without this, `git worktree add -b` will fail when the branch already
  # exists from a previous attempt.
  after_create: |
    set -e
    repo="$HOME/Developer/my-org/my-repo"
    branch="symposium/bug-{{ issue.safe_identifier }}"
    workdir="{{ workspace }}"
    [ -d "$repo/.git" ] || { echo "after_create: $repo is not a git repo" >&2; exit 2; }
    existing=$(git -C "$repo" worktree list --porcelain | awk -v b="refs/heads/$branch" '/^worktree /{w=$2} $1=="branch" && $2==b {print w; exit}')
    if [ -n "$existing" ]; then
      git -C "$repo" worktree unlock "$existing" 2>/dev/null || true
      git -C "$repo" worktree remove -f -f "$existing" 2>/dev/null || true
    fi
    git -C "$repo" branch -D "$branch" 2>/dev/null || true
    rm -rf "$workdir"
    git -C "$repo" worktree prune
    git -C "$repo" worktree add "$workdir" -b "$branch"
  # Runs when a workspace is being removed (terminal issue or age-based reaping).
  # `worktree remove` does NOT delete the branch — explicitly drop it so the
  # next attempt at the same issue identifier starts clean.
  before_remove: |
    repo="$HOME/Developer/my-org/my-repo"
    git -C "$repo" worktree remove --force {{ workspace }} 2>/dev/null || true
    git -C "$repo" branch -D symposium/bug-{{ issue.safe_identifier }} 2>/dev/null || true
  # Optional: runs before each agent attempt (retries included)
  # before_run: |
  #   git fetch origin main && git rebase origin/main
  # Optional: runs after each agent attempt
  # after_run: |
  #   echo "Agent finished with RUN_SUCCESS=$RUN_SUCCESS"
  # Optional: timeout for hook execution in milliseconds (default: 300000 = 5 min)
  # timeout_ms: 300000

agent:
  max_concurrent_agents: 3

codex:
  # Path to the Claude CLI binary (default: "claude-code app-server")
  command: "/usr/local/bin/claude"
  turn_timeout_ms: 3600000    # 1 hour max per agent session
  stall_timeout_ms: 300000    # 5 min with no activity -> stalled

server:
  port: 8080

# Optional: inject supplementary MCP servers into agent sessions.
# These are passed to the claude CLI via --mcp-config.
# Useful for giving agents access to Sentry, Datadog, PagerDuty, etc.
# mcp_servers:
#   sentry:
#     type: url  # "url" for HTTP MCP servers, "stdio" for command-based
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
#   # Passed to Sentry's MCP server as `projectSlugOrId` for hard server-side
#   # scoping; also used to derive an expected short-id prefix (e.g. `MY-PROJECT-`)
#   # for a defense-in-depth client-side filter.
#   project: "my-project"
#   mcp_url: "https://mcp.sentry.dev/mcp"  # Sentry MCP server (OAuth auth)
#   # Extra Sentry search filters appended after `is:unresolved` / `is:resolved`.
#   # Do NOT include `project:<slug>` here — the project is passed structurally
#   # above. Putting it in this string lets Sentry's NL parser treat the project
#   # as a soft hint, and unrelated projects can leak through.
#   query: "release:[my-app@1.7.*,my-app@1.8.*]"
#   min_events: 5                           # skip issues below this threshold
#   id_prefix: "SENTRY:"                    # default: "sentry:" — prefix for Sentry issue IDs

# Optional: run a pre-flight verification step before the main agent.
# When enabled, a separate agent session runs first to verify the issue is
# still valid (e.g. reproduce the bug, check if already fixed). If the agent
# writes a PREFLIGHT_SKIP file, the issue is skipped entirely -- no
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

# Optional: open PRs by triggering a workflow_dispatch GitHub Action in the
# target repo instead of calling `gh pr create` directly. When set, Symposium
# pushes the branch and runs `gh workflow run <workflow>` with the title and
# body as inputs. The Action opens the PR using GITHUB_TOKEN, so it appears
# authored by `github-actions[bot]` instead of the user Symposium is running
# under — useful when one human's account would otherwise own every PR and
# concentrate the review load on the same teammates.
#
# An example workflow you can drop into `.github/workflows/open-pr.yml` of your
# target repo lives at `examples/open-pr.yml` in the symposium repo.
#
# Caveat: PRs opened with the default GITHUB_TOKEN do NOT trigger downstream
# workflow runs (no recursive CI). If your repo needs CI on these PRs, swap in
# a PAT or GitHub App token inside the workflow itself.
# pr_creation:
#   workflow: open-pr.yml          # filename of the workflow_dispatch action in the target repo
#   branch_input: branch            # workflow input that receives the source branch
#   title_input: title              # workflow input that receives the PR title
#   body_input: body                # workflow input that receives the PR body (markdown)
#   poll_timeout_ms: 120000         # max time to wait for the workflow to open the PR
#   poll_interval_ms: 3000          # interval between PR-poll attempts
---
```

## Prompt Template

Everything after the YAML front matter closing `---` is a Liquid template.
It is rendered per-issue and sent to the agent as its initial prompt via stdin.

Available variables:
- `{{ issue.identifier }}` -- issue ID (e.g. "316205" or "sentry:MAIL-IOS-3B")
- `{{ issue.safe_identifier }}` -- branch-safe version of the ID (colons -> hyphens)
- `{{ issue.title }}` -- issue title
- `{{ issue.description }}` -- issue description/notes
- `{{ issue.status }}` -- current status
- `{{ issue.priority }}` -- priority level
- `{{ issue.url }}` -- link to the issue page
- `{{ issue.comments }}` -- formatted page comments (author + timestamp + body)
- `{{ issue.<property> }}` -- any extra Notion property (lowercased column name)
- `{{ attempt }}` -- retry attempt number (nil on first run)

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

The preflight agent runs with full access to the workspace, MCP servers, and tools --
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

- **`PR_TITLE`** -- A single line with the PR title. Should be a concise, human-readable
  summary of the actual change (not just the bug title). No conventional commit prefixes.
- **`PR_BODY.md`** -- Markdown PR body including investigation reasoning, what was changed
  and why, and a link back to the issue (e.g. `Fixes 316205`).

The **implementer agent** writes these files after committing its fix, since it has the
full context of what it investigated and why it chose its approach. The **review agent**
then updates them if it made any additional changes.

If the files are missing, Symposium falls back to a generic title/body based on the
issue ID and title.

These files are **not** committed to git -- they are read from disk and then cleaned up.

## PR Review Monitoring

When `pr_review.enabled` is set to `true`, Symposium monitors open PRs it created for
reviewer feedback. Each tick, it runs `gh pr view` to check for new comments or change
requests. If actionable feedback is detected, it spins up an agent in the **existing
workspace** to address the feedback -- no new workspace or branch is created.

Reviews are grouped per-author, and only the latest review state per author is considered.
This means if a reviewer requests changes and later approves, the approval supersedes the
earlier request and no fix agent is triggered.

The `reviewers` filter controls which reviewers' feedback triggers a response:

| Value | Behavior |
|-------|----------|
| `"all"` (default) | React to any reviewer |
| `"humans"` | Skip bot accounts (GitHub usernames ending in `[bot]`) |
| `["alice", "bob"]` | Only react to these specific GitHub usernames |
