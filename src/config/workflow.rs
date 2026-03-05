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
}
