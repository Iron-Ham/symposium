use crate::domain::issue::Issue;
use crate::error::{Error, Result};
use liquid::ParserBuilder;

/// Render the prompt template with issue data.
pub fn build_prompt(template_str: &str, issue: &Issue, attempt: Option<u32>) -> Result<String> {
    let parser = ParserBuilder::with_stdlib()
        .build()
        .map_err(|e| Error::Prompt(format!("failed to build liquid parser: {e}")))?;

    let template = parser
        .parse(template_str)
        .map_err(|e| Error::Prompt(format!("failed to parse template: {e}")))?;

    let mut globals = liquid::object!({
        "issue": {
            "identifier": issue.identifier,
            "title": issue.title,
            "description": issue.description.as_deref().unwrap_or(""),
            "status": issue.status,
            "priority": issue.priority.as_deref().unwrap_or(""),
            "url": issue.url.as_deref().unwrap_or(""),
        }
    });

    if let Some(attempt) = attempt {
        globals.insert(
            "attempt".into(),
            liquid::model::Value::scalar(attempt as i64),
        );
    }

    template
        .render(&globals)
        .map_err(|e| Error::Prompt(format!("failed to render template: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        };

        let template = "{% if attempt %}Retry #{{ attempt }}{% endif %} {{ issue.identifier }}";
        let result = build_prompt(template, &issue, Some(3)).unwrap();
        assert!(result.contains("Retry #3"));
        assert!(result.contains("TASK-456"));
    }
}
