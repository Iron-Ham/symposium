use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct ServiceConfig {
    pub tracker: TrackerConfig,
    pub polling: PollingConfig,
    pub workspace: WorkspaceConfig,
    pub hooks: HooksConfig,
    pub agent: AgentConfig,
    pub codex: CodexConfig,
    pub server: ServerConfig,
    pub preflight: PreflightConfig,
    pub review: ReviewConfig,
    pub pr_review: PrReviewConfig,
    pub pr_creation: PrCreationConfig,
    pub mcp_servers: HashMap<String, McpServerConfig>,
    pub sentry: SentryConfig,
    pub prompt_template: String,
}

fn default_sentry_mcp_url() -> String {
    "https://mcp.sentry.dev/mcp".to_string()
}

fn default_sentry_min_events() -> u64 {
    5
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SentryConfig {
    pub enabled: bool,
    pub org: String,
    pub project: String,
    /// MCP server URL for Sentry (uses OAuth for auth).
    #[serde(default = "default_sentry_mcp_url")]
    pub mcp_url: String,
    /// Extra Sentry search filters appended after `is:unresolved` (or
    /// `is:resolved`). The project filter is passed structurally via the MCP
    /// `projectSlugOrId` argument — do NOT include `project:<slug>` here, the
    /// Sentry MCP server's natural-language parser treats it as a soft hint
    /// only and will let issues from other projects leak through.
    /// Example: `"error.unhandled:true"` or
    /// `"release:[so.notion.Mail@1.7.*,so.notion.Mail@1.8.*]"`.
    pub query: String,
    #[serde(default = "default_sentry_min_events")]
    pub min_events: u64,
    /// Prefix for Sentry issue identifiers (default: "sentry:").
    #[serde(default = "default_sentry_id_prefix")]
    pub id_prefix: String,
}

fn default_sentry_id_prefix() -> String {
    "sentry:".to_string()
}

impl Default for SentryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            org: String::new(),
            project: String::new(),
            mcp_url: default_sentry_mcp_url(),
            query: String::new(),
            min_events: default_sentry_min_events(),
            id_prefix: default_sentry_id_prefix(),
        }
    }
}

fn default_mcp_type() -> String {
    "stdio".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct McpServerConfig {
    #[serde(rename = "type", default = "default_mcp_type")]
    pub server_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TrackerConfig {
    pub kind: String,
    pub mcp_command: String,
    pub mcp_url: Option<String>,
    pub database_id: String,
    pub active_states: Vec<String>,
    pub terminal_states: Vec<String>,
    pub property_id: String,
    pub property_title: String,
    pub property_status: String,
    pub property_priority: String,
    pub property_description: String,
    pub property_assignee: String,
    /// Filter issues to only those assigned to this user ID.
    pub assignee_user_id: Option<String>,
    /// Skip issues where this property is non-null (e.g. a linked PR relation).
    pub skip_if_set: Option<String>,
    /// Prefix prepended to the raw ID property value (e.g. "BUG-" → "BUG-316205").
    pub id_prefix: Option<String>,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            kind: "notion".to_string(),
            mcp_command: "npx -y @notionhq/notion-mcp-server".to_string(),
            mcp_url: None,
            database_id: String::new(),
            active_states: vec!["Todo".to_string(), "In Progress".to_string()],
            terminal_states: vec!["Done".to_string(), "Cancelled".to_string()],
            property_id: "ID".to_string(),
            property_title: "Name".to_string(),
            property_status: "Status".to_string(),
            property_priority: "Priority".to_string(),
            property_description: "Description".to_string(),
            property_assignee: "Assignee".to_string(),
            assignee_user_id: None,
            skip_if_set: None,
            id_prefix: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PollingConfig {
    pub interval_ms: u64,
}

impl Default for PollingConfig {
    fn default() -> Self {
        Self { interval_ms: 30000 }
    }
}

impl PollingConfig {
    pub fn interval(&self) -> Duration {
        Duration::from_millis(self.interval_ms)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct WorkspaceConfig {
    pub root: String,
    /// Subdirectory within the workspace to use as the agent's working directory.
    pub agent_subdirectory: Option<String>,
    /// Reap workspaces whose top-level mtime is older than this many days.
    /// Running sessions and workspaces with tracked open PRs are always skipped.
    /// `None` disables the reaper.
    pub max_age_days: Option<u64>,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            root: "~/symposium_workspaces".to_string(),
            agent_subdirectory: None,
            max_age_days: None,
        }
    }
}

impl WorkspaceConfig {
    pub fn max_age(&self) -> Option<Duration> {
        self.max_age_days
            .map(|d| Duration::from_secs(d * 24 * 60 * 60))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct HooksConfig {
    pub after_create: Option<String>,
    pub before_run: Option<String>,
    pub after_run: Option<String>,
    /// Optional shell hook rendered and run from the workspace's parent
    /// directory before the workspace itself is deleted. Typical use:
    /// `git -C <repo> worktree remove --force {{ workspace }}` so worktree
    /// metadata is pruned along with the files. If the hook fails we still
    /// fall back to `remove_dir_all` so disk is always reclaimed.
    pub before_remove: Option<String>,
    pub timeout_ms: u64,
}

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            after_create: None,
            before_run: None,
            after_run: None,
            before_remove: None,
            timeout_ms: 300_000,
        }
    }
}

impl HooksConfig {
    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    pub max_concurrent_agents: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_concurrent_agents: 5,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CodexConfig {
    pub command: String,
    pub turn_timeout_ms: u64,
    pub stall_timeout_ms: u64,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            command: "claude-code app-server".to_string(),
            turn_timeout_ms: 3_600_000,
            stall_timeout_ms: 300_000,
        }
    }
}

impl CodexConfig {
    pub fn turn_timeout(&self) -> Duration {
        Duration::from_millis(self.turn_timeout_ms)
    }

    pub fn stall_timeout(&self) -> Duration {
        Duration::from_millis(self.stall_timeout_ms)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self { port: 8080 }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PreflightConfig {
    /// Whether to run a pre-flight verification step before the main agent.
    /// Defaults to false.
    pub enabled: bool,
    /// Liquid template for the pre-flight agent prompt.
    /// The agent should verify that the issue is still valid/reproducible.
    /// If the agent writes a `PREFLIGHT_SKIP` file, the issue is skipped.
    pub prompt_template: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ReviewConfig {
    /// Whether to run the review step at all. Defaults to true.
    pub enabled: bool,
    /// Liquid template for the review prompt. If empty, uses the built-in default.
    pub prompt_template: String,
    /// Optional shell hook to run before the review agent starts
    /// (e.g. to generate a lint/review report the agent can read).
    pub before_review: Option<String>,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            prompt_template: String::new(),
            before_review: None,
        }
    }
}

/// Which reviewers' feedback should trigger a fix agent.
#[derive(Debug, Clone, Default)]
pub enum ReviewerFilter {
    /// React to any reviewer's changes-requested.
    #[default]
    All,
    /// Skip bot accounts (GitHub usernames ending in `[bot]`).
    Humans,
    /// Only react to these specific GitHub usernames.
    Specific(Vec<String>),
}

impl<'de> Deserialize<'de> for ReviewerFilter {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct FilterVisitor;

        impl<'de> serde::de::Visitor<'de> for FilterVisitor {
            type Value = ReviewerFilter;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str(r#""all", "humans", or a list of GitHub usernames"#)
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<ReviewerFilter, E> {
                match v {
                    "all" => Ok(ReviewerFilter::All),
                    "humans" => Ok(ReviewerFilter::Humans),
                    other => Err(E::custom(format!(
                        "unknown reviewer filter: \"{other}\", expected \"all\", \"humans\", or a list"
                    ))),
                }
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<ReviewerFilter, A::Error> {
                let mut names = Vec::new();
                while let Some(name) = seq.next_element::<String>()? {
                    names.push(name);
                }
                Ok(ReviewerFilter::Specific(names))
            }
        }

        deserializer.deserialize_any(FilterVisitor)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PrReviewConfig {
    /// Whether to monitor open PRs for review feedback. Defaults to false.
    pub enabled: bool,
    /// Liquid template for the PR review response prompt. If empty, uses the built-in default.
    pub prompt_template: String,
    /// Which reviewers to respond to: "all", "humans", or a list of GitHub usernames.
    pub reviewers: ReviewerFilter,
}

impl Default for PrReviewConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            prompt_template: String::new(),
            reviewers: ReviewerFilter::All,
        }
    }
}

/// How Symposium should open the PR after the agent commits its work.
///
/// By default Symposium calls `gh pr create --draft` directly. That uses the
/// token Symposium itself was authenticated with, so the PR is authored by
/// that user and review notifications go to whoever GitHub picks from CODEOWNERS
/// for that user. In setups where Symposium runs as a single human's account,
/// every PR concentrates review workload on the same handful of teammates.
///
/// Setting `workflow` switches to a `workflow_dispatch` flow: Symposium pushes
/// the branch and triggers the named GitHub Action in the target repo, passing
/// the title and body as inputs. The Action opens the PR using `GITHUB_TOKEN`,
/// so it appears authored by `github-actions[bot]` and review can be routed via
/// CODEOWNERS independent of whoever is running Symposium.
///
/// Caveat: PRs opened with `GITHUB_TOKEN` do not trigger downstream workflow
/// runs (no recursive CI). Repos that need CI on bot-opened PRs should use a
/// PAT or a GitHub App token inside the workflow itself.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PrCreationConfig {
    /// Filename (or path) of the workflow_dispatch GitHub Action in the target
    /// repo that opens the PR (e.g. "open-pr.yml"). When empty, Symposium falls
    /// back to running `gh pr create --draft` directly.
    pub workflow: String,
    /// Name of the workflow input that receives the source branch.
    pub branch_input: String,
    /// Name of the workflow input that receives the PR title.
    pub title_input: String,
    /// Name of the workflow input that receives the PR body.
    pub body_input: String,
    /// Maximum time to wait for the workflow to open the PR before giving up.
    /// Specified in milliseconds. The branch is polled with `gh pr list --head`.
    pub poll_timeout_ms: u64,
    /// Interval between PR-poll attempts, in milliseconds.
    pub poll_interval_ms: u64,
}

impl Default for PrCreationConfig {
    fn default() -> Self {
        Self {
            workflow: String::new(),
            branch_input: "branch".to_string(),
            title_input: "title".to_string(),
            body_input: "body".to_string(),
            poll_timeout_ms: 120_000,
            poll_interval_ms: 3_000,
        }
    }
}

impl PrCreationConfig {
    /// True when a workflow_dispatch PR-creation flow has been configured.
    pub fn is_workflow_dispatch(&self) -> bool {
        !self.workflow.is_empty()
    }

    pub fn poll_timeout(&self) -> Duration {
        Duration::from_millis(self.poll_timeout_ms)
    }

    pub fn poll_interval(&self) -> Duration {
        Duration::from_millis(self.poll_interval_ms)
    }
}
