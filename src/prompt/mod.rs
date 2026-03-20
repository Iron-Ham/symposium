use crate::domain::issue::Issue;
use crate::error::{Error, Result};
use liquid::ParserBuilder;

/// Render the prompt template with issue data.
pub fn build_prompt(template_str: &str, issue: &Issue, attempt: Option<u32>) -> Result<String> {
    build_prompt_full(template_str, issue, attempt, None, None)
}

/// Render a template with issue data and an optional workspace path.
pub fn build_prompt_with_workspace(
    template_str: &str,
    issue: &Issue,
    attempt: Option<u32>,
    workspace: Option<&str>,
) -> Result<String> {
    build_prompt_full(template_str, issue, attempt, workspace, None)
}

/// Render a template with issue data, workspace path, and base branch.
pub fn build_prompt_full(
    template_str: &str,
    issue: &Issue,
    attempt: Option<u32>,
    workspace: Option<&str>,
    base_branch: Option<&str>,
) -> Result<String> {
    let parser = ParserBuilder::with_stdlib()
        .build()
        .map_err(|e| Error::Prompt(format!("failed to build liquid parser: {e}")))?;

    let template = parser
        .parse(template_str)
        .map_err(|e| Error::Prompt(format!("failed to parse template: {e}")))?;

    // Provide a branch-safe version of the identifier (colons → hyphens, etc.)
    let safe_id = crate::workspace::safety::sanitize_key(&issue.identifier);

    // Format comments into a readable string for template access
    let comments_text = if issue.comments.is_empty() {
        String::new()
    } else {
        issue
            .comments
            .iter()
            .map(|c| {
                let ts = c
                    .created_at
                    .as_deref()
                    .map(|t| format!(" ({t})"))
                    .unwrap_or_default();
                format!("**{}**{}: {}", c.author, ts, c.body)
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    };

    let mut issue_obj = liquid::object!({
        "identifier": issue.identifier,
        "safe_identifier": safe_id,
        "title": issue.title,
        "description": issue.description.as_deref().unwrap_or(""),
        "status": issue.status,
        "priority": issue.priority.as_deref().unwrap_or(""),
        "url": issue.url.as_deref().unwrap_or(""),
        "comments": comments_text,
    });

    // Merge extra properties so templates can use e.g. {{ issue.platform }}
    for (key, val) in &issue.extra {
        issue_obj.insert(key.clone().into(), liquid::model::Value::scalar(val.clone()));
    }

    let mut globals = liquid::object!({
        "issue": issue_obj,
    });

    if let Some(attempt) = attempt {
        globals.insert(
            "attempt".into(),
            liquid::model::Value::scalar(attempt as i64),
        );
    }

    if let Some(ws) = workspace {
        globals.insert(
            "workspace".into(),
            liquid::model::Value::scalar(ws.to_string()),
        );
    }

    if let Some(branch) = base_branch {
        globals.insert(
            "base_branch".into(),
            liquid::model::Value::scalar(branch.to_string()),
        );
    }

    template
        .render(&globals)
        .map_err(|e| Error::Prompt(format!("failed to render template: {e}")))
}

/// Resolve the agent working directory from a workspace dir and subdirectory template.
///
/// If `subdirectory_template` is `None` or renders to empty/whitespace, returns
/// `workspace_dir` directly. Otherwise, renders the template with issue context
/// (supporting Liquid expressions like `{% if issue.title contains '[iOS]' %}mail-ios{% endif %}`)
/// and joins the result with `workspace_dir`.
pub fn resolve_agent_dir(
    workspace_dir: &std::path::Path,
    subdirectory_template: Option<&str>,
    issue: &Issue,
) -> std::path::PathBuf {
    let Some(tmpl) = subdirectory_template else {
        return workspace_dir.to_path_buf();
    };
    if tmpl.is_empty() {
        return workspace_dir.to_path_buf();
    }

    match build_prompt(tmpl, issue, None) {
        Ok(rendered) => {
            let rendered = rendered.trim();
            if rendered.is_empty() {
                workspace_dir.to_path_buf()
            } else {
                workspace_dir.join(rendered)
            }
        }
        Err(e) => {
            tracing::warn!("failed to render agent_subdirectory template: {e}, using workspace root");
            workspace_dir.to_path_buf()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_basic_render() {
        let issue = Issue {
            identifier: "TASK-123".to_string(),
            title: "Fix the bug".to_string(),
            description: Some("Something is broken".to_string()),
            status: "Todo".to_string(),
            priority: Some("High".to_string()),
            url: Some("https://notion.so/page".to_string()),
            notion_page_id: Some("abc123".to_string()),
            blockers: vec![],
            source: "notion".to_string(),
            extra: HashMap::new(),
            comments: vec![],
            workflow_id: String::new(),
        };

        let template = "Work on {{ issue.identifier }}: {{ issue.title }}\n{{ issue.description }}";
        let result = build_prompt(template, &issue, None).unwrap();
        assert_eq!(result, "Work on TASK-123: Fix the bug\nSomething is broken");
    }

    #[test]
    fn test_attempt_render() {
        let issue = Issue {
            identifier: "TASK-456".to_string(),
            title: "Another task".to_string(),
            description: None,
            status: "In Progress".to_string(),
            priority: None,
            url: None,
            notion_page_id: None,
            blockers: vec![],
            source: "notion".to_string(),
            extra: HashMap::new(),
            comments: vec![],
            workflow_id: String::new(),
        };

        let template = "{% if attempt %}Retry #{{ attempt }}{% endif %} {{ issue.identifier }}";
        let result = build_prompt(template, &issue, Some(3)).unwrap();
        assert!(result.contains("Retry #3"));
        assert!(result.contains("TASK-456"));
    }

    #[test]
    fn test_extra_properties_render() {
        let mut extra = HashMap::new();
        extra.insert("platform".to_string(), "iOS".to_string());
        let issue = Issue {
            identifier: "BUG-1".to_string(),
            title: "Crash".to_string(),
            description: None,
            status: "On Deck".to_string(),
            priority: Some("P1".to_string()),
            url: None,
            notion_page_id: None,
            blockers: vec![],
            source: "notion".to_string(),
            extra,
            comments: vec![],
            workflow_id: String::new(),
        };

        let template = "Platform: {{ issue.platform }}";
        let result = build_prompt(template, &issue, None).unwrap();
        assert_eq!(result, "Platform: iOS");
    }

    #[test]
    fn test_comments_render() {
        use crate::domain::issue::Comment;

        let issue = Issue {
            identifier: "TASK-789".to_string(),
            title: "Auth bug".to_string(),
            description: None,
            status: "Todo".to_string(),
            priority: None,
            url: None,
            notion_page_id: None,
            blockers: vec![],
            source: "notion".to_string(),
            extra: HashMap::new(),
            comments: vec![
                Comment {
                    author: "Alice".to_string(),
                    body: "This happens on login".to_string(),
                    created_at: Some("2026-03-01T10:00:00Z".to_string()),
                },
                Comment {
                    author: "Bob".to_string(),
                    body: "Confirmed on staging".to_string(),
                    created_at: None,
                },
            ],
            workflow_id: String::new(),
        };

        let template = "{% if issue.comments != blank %}Comments:\n{{ issue.comments }}{% endif %}";
        let result = build_prompt(template, &issue, None).unwrap();
        assert!(result.contains("**Alice** (2026-03-01T10:00:00Z): This happens on login"));
        assert!(result.contains("**Bob**: Confirmed on staging"));
    }

    #[test]
    fn test_empty_comments_render() {
        let issue = Issue {
            identifier: "TASK-000".to_string(),
            title: "No comments".to_string(),
            description: None,
            status: "Todo".to_string(),
            priority: None,
            url: None,
            notion_page_id: None,
            blockers: vec![],
            source: "notion".to_string(),
            extra: HashMap::new(),
            comments: vec![],
            workflow_id: String::new(),
        };

        let template = "{% if issue.comments != blank %}Comments:\n{{ issue.comments }}{% else %}No comments{% endif %}";
        let result = build_prompt(template, &issue, None).unwrap();
        assert_eq!(result, "No comments");
    }

    #[test]
    fn test_base_branch_render() {
        let issue = Issue {
            identifier: "TASK-1".to_string(),
            title: "Test".to_string(),
            description: None,
            status: "Todo".to_string(),
            priority: None,
            url: None,
            notion_page_id: None,
            blockers: vec![],
            source: "notion".to_string(),
            extra: HashMap::new(),
            comments: vec![],
            workflow_id: String::new(),
        };

        let template = "git rebase origin/{{ base_branch }}";
        let result = build_prompt_full(template, &issue, None, None, Some("symposium/task-TASK-99")).unwrap();
        assert_eq!(result, "git rebase origin/symposium/task-TASK-99");
    }

    #[test]
    fn test_resolve_agent_dir_with_liquid() {
        use std::path::PathBuf;

        let ios_issue = Issue {
            identifier: "TASK-1".to_string(),
            title: "[iOS] Gate: swipe gesture".to_string(),
            description: None,
            status: "Todo".to_string(),
            priority: None,
            url: None,
            notion_page_id: None,
            blockers: vec![],
            source: "notion".to_string(),
            extra: HashMap::new(),
            comments: vec![],
            workflow_id: String::new(),
        };
        let backend_issue = Issue {
            title: "[Backend] API endpoints".to_string(),
            ..ios_issue.clone()
        };

        let ws = PathBuf::from("/tmp/workspace");
        let tmpl = "{% if issue.title contains '[iOS]' %}mail-ios{% endif %}";

        // iOS task → subdirectory
        let dir = resolve_agent_dir(&ws, Some(tmpl), &ios_issue);
        assert_eq!(dir, PathBuf::from("/tmp/workspace/mail-ios"));

        // Backend task → workspace root (template renders to empty)
        let dir = resolve_agent_dir(&ws, Some(tmpl), &backend_issue);
        assert_eq!(dir, PathBuf::from("/tmp/workspace"));

        // No template → workspace root
        let dir = resolve_agent_dir(&ws, None, &ios_issue);
        assert_eq!(dir, PathBuf::from("/tmp/workspace"));
    }
}
