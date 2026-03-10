use super::env;
use super::schema::ServiceConfig;
use crate::error::{Error, Result};
use std::path::Path;

/// Parse a WORKFLOW.md file into a ServiceConfig (with prompt_template populated).
///
/// Expected format:
/// ```text
/// ---
/// tracker:
///   kind: notion
/// ---
/// You are working on issue {{ issue.identifier }}...
/// ```
pub fn parse_workflow(content: &str) -> Result<(ServiceConfig, String)> {
    let trimmed = content.trim();
    if !trimmed.starts_with("---") {
        return Err(Error::Workflow(
            "WORKFLOW.md must start with --- front matter delimiter".to_string(),
        ));
    }

    // Find the closing --- delimiter (skip the opening one)
    let after_first = &trimmed[3..];
    let end_idx = after_first.find("\n---").ok_or_else(|| {
        Error::Workflow("missing closing --- front matter delimiter".to_string())
    })?;

    let yaml_str = &after_first[..end_idx];
    let prompt_start = 3 + end_idx + 4; // skip "---" + "\n---"
    let prompt_template = trimmed[prompt_start..].trim().to_string();

    let mut config: ServiceConfig =
        serde_yaml::from_str(yaml_str).map_err(|e| Error::ConfigParse(e.to_string()))?;

    config.prompt_template = prompt_template.clone();
    env::expand_config(&mut config);

    Ok((config, prompt_template))
}

/// Parse a WORKFLOW.md file from disk into a ServiceConfig.
pub fn parse_workflow_file(path: &Path) -> Result<ServiceConfig> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| Error::Workflow(format!("failed to read {}: {}", path.display(), e)))?;
    let (config, _) = parse_workflow(&content)?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_workflow() {
        let content = r#"---
tracker:
  kind: notion
  database_id: abc123
polling:
  interval_ms: 5000
---
You are working on issue {{ issue.identifier }}.
"#;
        let (config, prompt) = parse_workflow(content).unwrap();
        assert_eq!(config.tracker.kind, "notion");
        assert_eq!(config.tracker.database_id, "abc123");
        assert_eq!(config.polling.interval_ms, 5000);
        assert!(prompt.contains("{{ issue.identifier }}"));
    }

    #[test]
    fn parse_defaults_applied() {
        let content = "---\n---\nPrompt here.";
        let (config, prompt) = parse_workflow(content).unwrap();
        assert_eq!(config.tracker.kind, "notion");
        assert_eq!(config.server.port, 8080);
        assert_eq!(prompt, "Prompt here.");
    }

    #[test]
    fn parse_missing_front_matter() {
        let result = parse_workflow("no front matter here");
        assert!(result.is_err());
    }

    #[test]
    fn parse_missing_closing_delimiter() {
        let result = parse_workflow("---\ntracker:\n  kind: notion\n");
        assert!(result.is_err());
    }

    #[test]
    fn parse_preflight_config() {
        let content = r#"---
preflight:
  enabled: true
  prompt_template: "Verify {{ issue.identifier }}: {{ issue.title }}"
---
Prompt here."#;
        let (config, _) = parse_workflow(content).unwrap();
        assert!(config.preflight.enabled);
        assert_eq!(
            config.preflight.prompt_template,
            "Verify {{ issue.identifier }}: {{ issue.title }}"
        );
    }

    #[test]
    fn parse_preflight_config_defaults() {
        let content = "---\n---\nPrompt here.";
        let (config, _) = parse_workflow(content).unwrap();
        assert!(!config.preflight.enabled);
        assert!(config.preflight.prompt_template.is_empty());
    }

    #[test]
    fn parse_review_config() {
        let content = r#"---
review:
  enabled: false
  prompt_template: "Review {{ issue.identifier }} carefully."
  before_review: "npx my-linter"
---
Prompt here."#;
        let (config, _) = parse_workflow(content).unwrap();
        assert!(!config.review.enabled);
        assert_eq!(config.review.prompt_template, "Review {{ issue.identifier }} carefully.");
        assert_eq!(config.review.before_review.as_deref(), Some("npx my-linter"));
    }

    #[test]
    fn parse_review_config_defaults() {
        let content = "---\n---\nPrompt here.";
        let (config, _) = parse_workflow(content).unwrap();
        assert!(config.review.enabled);
        assert!(config.review.prompt_template.is_empty());
        assert!(config.review.before_review.is_none());
    }

    #[test]
    fn parse_mcp_server_http() {
        let content = r#"---
mcp_servers:
  sentry:
    type: http
    url: "https://mcp.sentry.dev/mcp"
---
Prompt here."#;
        let (config, _) = parse_workflow(content).unwrap();
        assert_eq!(config.mcp_servers.len(), 1);
        let sentry = &config.mcp_servers["sentry"];
        assert_eq!(sentry.server_type, "http");
        assert_eq!(sentry.url.as_deref(), Some("https://mcp.sentry.dev/mcp"));
    }

    #[test]
    fn parse_mcp_server_stdio_with_args_and_env() {
        let content = r#"---
mcp_servers:
  custom-linter:
    type: stdio
    command: "npx"
    args: ["-y", "@my-org/linter-mcp"]
    env:
      API_KEY: "test-key"
---
Prompt here."#;
        let (config, _) = parse_workflow(content).unwrap();
        let linter = &config.mcp_servers["custom-linter"];
        assert_eq!(linter.server_type, "stdio");
        assert_eq!(linter.command.as_deref(), Some("npx"));
        assert_eq!(
            linter.args.as_deref(),
            Some(["-y".to_string(), "@my-org/linter-mcp".to_string()].as_slice())
        );
        assert_eq!(
            linter.env.as_ref().unwrap().get("API_KEY").unwrap(),
            "test-key"
        );
    }

    #[test]
    fn parse_mcp_servers_empty_by_default() {
        let content = "---\n---\nPrompt here.";
        let (config, _) = parse_workflow(content).unwrap();
        assert!(config.mcp_servers.is_empty());
    }

    #[test]
    fn parse_sentry_config() {
        let content = r#"---
sentry:
  enabled: true
  org: "notion"
  project: "mail-ios"
  mcp_url: "https://mcp.sentry.dev/mcp"
  query: "release:[so.notion.Mail@1.7.*,so.notion.Mail@1.8.*]"
  min_events: 10
---
Prompt here."#;
        let (config, _) = parse_workflow(content).unwrap();
        assert!(config.sentry.enabled);
        assert_eq!(config.sentry.org, "notion");
        assert_eq!(config.sentry.project, "mail-ios");
        assert_eq!(config.sentry.mcp_url, "https://mcp.sentry.dev/mcp");
        assert_eq!(
            config.sentry.query,
            "release:[so.notion.Mail@1.7.*,so.notion.Mail@1.8.*]"
        );
        assert_eq!(config.sentry.min_events, 10);
    }

    #[test]
    fn parse_sentry_config_defaults() {
        let content = "---\n---\nPrompt here.";
        let (config, _) = parse_workflow(content).unwrap();
        assert!(!config.sentry.enabled);
        assert!(config.sentry.org.is_empty());
        assert!(config.sentry.query.is_empty());
        assert_eq!(config.sentry.min_events, 5);
        assert_eq!(config.sentry.mcp_url, "https://mcp.sentry.dev/mcp");
    }

    #[test]
    fn parse_sentry_config_backward_compat() {
        // Existing config without sentry block should still parse
        let content = r#"---
tracker:
  kind: notion
  database_id: abc123
---
Prompt here."#;
        let (config, _) = parse_workflow(content).unwrap();
        assert!(!config.sentry.enabled);
        assert_eq!(config.tracker.kind, "notion");
    }

    #[test]
    fn parse_pr_review_config_defaults() {
        let content = "---\n---\nPrompt here.";
        let (config, _) = parse_workflow(content).unwrap();
        assert!(!config.pr_review.enabled);
        assert!(config.pr_review.prompt_template.is_empty());
        assert!(matches!(
            config.pr_review.reviewers,
            crate::config::schema::ReviewerFilter::All
        ));
    }

    #[test]
    fn parse_pr_review_config_enabled() {
        let content = r#"---
pr_review:
  enabled: true
  reviewers: humans
---
Prompt here."#;
        let (config, _) = parse_workflow(content).unwrap();
        assert!(config.pr_review.enabled);
        assert!(matches!(
            config.pr_review.reviewers,
            crate::config::schema::ReviewerFilter::Humans
        ));
    }

    #[test]
    fn parse_pr_review_config_specific_reviewers() {
        let content = r#"---
pr_review:
  enabled: true
  reviewers:
    - alice
    - bob
---
Prompt here."#;
        let (config, _) = parse_workflow(content).unwrap();
        assert!(config.pr_review.enabled);
        match &config.pr_review.reviewers {
            crate::config::schema::ReviewerFilter::Specific(names) => {
                assert_eq!(names, &["alice", "bob"]);
            }
            other => panic!("expected Specific, got {other:?}"),
        }
    }

    #[test]
    fn parse_pr_review_config_with_template() {
        let content = r#"---
pr_review:
  enabled: true
  prompt_template: "Fix PR #{{ issue.pr_number }} for {{ issue.identifier }}."
---
Prompt here."#;
        let (config, _) = parse_workflow(content).unwrap();
        assert!(config.pr_review.enabled);
        assert!(config.pr_review.prompt_template.contains("{{ issue.pr_number }}"));
    }
}
