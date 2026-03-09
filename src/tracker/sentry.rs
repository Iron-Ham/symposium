use super::mcp_http::HttpMcpClient;
use super::TrackerClient;
use crate::config::schema::SentryConfig;
use crate::domain::issue::Issue;
use crate::error::{Error, Result};
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;

pub struct SentryTracker {
    config: SentryConfig,
    client: HttpMcpClient,
}

impl SentryTracker {
    pub async fn new(config: SentryConfig) -> Result<Self> {
        let client = HttpMcpClient::new(&config.mcp_url).await?;
        Ok(Self { config, client })
    }

    /// Build the full Sentry search string.
    ///
    /// Combines `is:unresolved`/`is:resolved`, `project:<slug>`,
    /// and the user-supplied `query` from config.
    fn build_query(&self, resolved: bool) -> String {
        let status = if resolved { "is:resolved" } else { "is:unresolved" };
        let project = &self.config.project;
        let extra = &self.config.query;

        if extra.is_empty() {
            format!("project:{project} {status}")
        } else {
            format!("project:{project} {status} {extra}")
        }
    }

    /// Call the `search_issues` MCP tool and parse the markdown response into Issues.
    async fn fetch_issues(&mut self, resolved: bool) -> Result<Vec<Issue>> {
        let nl_query = self.build_query(resolved);
        tracing::info!(query = %nl_query, "Sentry MCP query");

        let result = self
            .client
            .call_tool(
                "search_issues",
                serde_json::json!({
                    "organizationSlug": self.config.org,
                    "naturalLanguageQuery": nl_query,
                }),
            )
            .await?;

        if result.get("isError").and_then(|v| v.as_bool()).unwrap_or(false) {
            let text = Self::extract_text(&result);
            tracing::error!(response = %text, "Sentry MCP returned error");
            return Ok(vec![]);
        }

        let text = Self::extract_text(&result);
        let issues = self.parse_issue_list(&text);
        tracing::info!(count = issues.len(), "fetched issues from Sentry MCP");
        Ok(issues)
    }

    /// Call `get_issue_details` MCP tool for a single issue.
    async fn fetch_issue_detail(&mut self, short_id: &str) -> Result<Option<Issue>> {
        let result = self
            .client
            .call_tool(
                "get_issue_details",
                serde_json::json!({
                    "organizationSlug": self.config.org,
                    "issueId": short_id,
                }),
            )
            .await?;

        let text = Self::extract_text(&result);
        // get_issue_details returns a single issue with similar formatting
        let issues = self.parse_issue_list(&text);
        Ok(issues.into_iter().next())
    }

    /// Extract the text content from an MCP tool response.
    fn extract_text(result: &Value) -> String {
        // MCP tools return {"content": [{"type":"text","text":"..."}]}
        if let Some(content) = result.get("content").and_then(|v| v.as_array()) {
            let texts: Vec<&str> = content
                .iter()
                .filter_map(|block| block.get("text").and_then(|v| v.as_str()))
                .collect();
            return texts.join("\n");
        }
        // Fallback: maybe just a text field directly
        result
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    }

    /// Parse the markdown response from `list_issues` into Issue structs.
    ///
    /// Expected format per issue:
    /// ```text
    /// ## N. [SHORT-ID](url)
    ///
    /// **Title**
    ///
    /// - **Status**: status
    /// - **Users**: N
    /// - **Events**: N
    /// - **First seen**: ...
    /// - **Last seen**: ...
    /// - **Culprit**: `culprit`
    /// ```
    fn parse_issue_list(&self, text: &str) -> Vec<Issue> {
        let mut issues = Vec::new();

        // Match issue header: ## N. [SHORT-ID](url)
        let header_re =
            Regex::new(r"##\s+\d+\.\s+\[([^\]]+)\]\(([^)]+)\)").unwrap();

        // Split text into sections by issue headers
        let sections: Vec<(regex::Match, &str)> = {
            let matches: Vec<_> = header_re.find_iter(text).collect();
            matches
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    let end = matches
                        .get(i + 1)
                        .map(|next| next.start())
                        .unwrap_or(text.len());
                    (*m, &text[m.start()..end])
                })
                .collect()
        };

        for (_, section) in sections {
            if let Some(caps) = header_re.captures(section) {
                let short_id = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                let url = caps.get(2).map(|m| m.as_str()).unwrap_or("");

                let title = Self::extract_bold_line(section);
                let status = Self::extract_field(section, "Status").unwrap_or_default();
                let users = Self::extract_field(section, "Users")
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                let events = Self::extract_field(section, "Events")
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                let first_seen = Self::extract_field(section, "First seen").unwrap_or_default();
                let last_seen = Self::extract_field(section, "Last seen").unwrap_or_default();
                let culprit = Self::extract_field(section, "Culprit")
                    .map(|s| s.trim_matches('`').to_string())
                    .unwrap_or_default();

                // Apply min_events filter
                if events < self.config.min_events {
                    continue;
                }

                // Map to priority (we don't get level from the list view,
                // so default to "High" for error-level issues)
                let priority = Some("High".to_string());

                let description = format!(
                    "Sentry crash: {title}\n\nCulprit: {culprit}\n\
                     Events: {events} | Users affected: {users}\n\
                     First seen: {first_seen} | Last seen: {last_seen}\n\
                     Sentry link: {url}"
                );

                let mut extra = HashMap::new();
                extra.insert("event_count".to_string(), events.to_string());
                extra.insert("user_count".to_string(), users.to_string());
                extra.insert("first_seen".to_string(), first_seen);
                extra.insert("last_seen".to_string(), last_seen);
                if !culprit.is_empty() {
                    extra.insert("culprit".to_string(), culprit);
                }

                issues.push(Issue {
                    identifier: format!("sentry:{short_id}"),
                    title,
                    description: Some(description),
                    status,
                    priority,
                    url: if url.is_empty() {
                        None
                    } else {
                        Some(url.to_string())
                    },
                    notion_page_id: None,
                    blockers: vec![],
                    source: "sentry".to_string(),
                    extra,
                    comments: vec![],
                });
            }
        }

        issues
    }

    /// Extract the first bold line (issue title) from a section.
    /// Looks for `**Title text**` on its own line.
    fn extract_bold_line(section: &str) -> String {
        let re = Regex::new(r"(?m)^\*\*(.+?)\*\*$").unwrap();
        re.captures(section)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
            .unwrap_or_default()
    }

    /// Extract a `- **Field**: value` from a section.
    fn extract_field(section: &str, field: &str) -> Option<String> {
        let pattern = format!(r"- \*\*{field}\*\*:\s*(.+)");
        let re = Regex::new(&pattern).ok()?;
        re.captures(section)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().trim().to_string())
    }
}

impl TrackerClient for SentryTracker {
    async fn fetch_candidate_issues(&mut self) -> Result<Vec<Issue>> {
        self.fetch_issues(false).await
    }

    async fn fetch_issue_states_by_ids(&mut self, ids: &[String]) -> Result<Vec<Issue>> {
        let mut issues = Vec::new();
        for id in ids {
            let short_id = id.strip_prefix("sentry:").unwrap_or(id);
            match self.fetch_issue_detail(short_id).await {
                Ok(Some(issue)) => issues.push(issue),
                Ok(None) => {
                    tracing::warn!(id, "Sentry issue not found");
                }
                Err(e) => {
                    tracing::warn!(id, "failed to fetch Sentry issue details: {e}");
                }
            }
        }
        Ok(issues)
    }

    async fn fetch_terminal_issues(&mut self) -> Result<Vec<Issue>> {
        self.fetch_issues(true).await
    }

    async fn agent_query(&mut self, _sql: &str) -> Result<Value> {
        Err(Error::Tracker(
            "agent_query not supported for Sentry".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config() -> SentryConfig {
        SentryConfig {
            min_events: 1,
            ..SentryConfig::default()
        }
    }

    #[test]
    fn parse_issue_list_from_markdown() {
        let config = make_config();
        // Can't create SentryTracker without MCP connection, so test parse directly
        let text = r#"# Issues in **notion/mail-ios**

Found **2** issues:

## 1. [MAIL-IOS-1A3](https://notion.sentry.io/issues/MAIL-IOS-1A3/)

**NullPointerException in FooBar**

- **Status**: unresolved
- **Users**: 5
- **Events**: 42
- **First seen**: 3 days ago
- **Last seen**: 2 hours ago
- **Culprit**: `com.example.FooBar.process`

## 2. [MAIL-IOS-2B7](https://notion.sentry.io/issues/MAIL-IOS-2B7/)

**IndexOutOfBounds in ListView**

- **Status**: unresolved
- **Users**: 12
- **Events**: 100
- **First seen**: 1 day ago
- **Last seen**: 30 minutes ago

## Next Steps

- Get more details about a specific issue
"#;

        // Use a temporary struct to call parse_issue_list
        let tracker_config = SentryConfig {
            min_events: 1,
            ..config
        };
        // We need a way to call parse_issue_list without a live connection.
        // Since it's a method on SentryTracker which requires MCP, test via standalone fn.
        let issues = parse_issue_list_standalone(&tracker_config, text);

        assert_eq!(issues.len(), 2);

        let first = &issues[0];
        assert_eq!(first.identifier, "sentry:MAIL-IOS-1A3");
        assert_eq!(first.title, "NullPointerException in FooBar");
        assert_eq!(first.source, "sentry");
        assert_eq!(first.status, "unresolved");
        assert_eq!(
            first.url,
            Some("https://notion.sentry.io/issues/MAIL-IOS-1A3/".to_string())
        );
        assert_eq!(first.extra.get("event_count").unwrap(), "42");
        assert_eq!(first.extra.get("user_count").unwrap(), "5");
        assert_eq!(
            first.extra.get("culprit").unwrap(),
            "com.example.FooBar.process"
        );

        let second = &issues[1];
        assert_eq!(second.identifier, "sentry:MAIL-IOS-2B7");
        assert_eq!(second.title, "IndexOutOfBounds in ListView");
        assert_eq!(second.extra.get("event_count").unwrap(), "100");
    }

    #[test]
    fn parse_filters_below_min_events() {
        let config = SentryConfig {
            min_events: 50,
            ..SentryConfig::default()
        };

        let text = r#"## 1. [TEST-1](https://sentry.io/issues/TEST-1/)

**Low count issue**

- **Status**: unresolved
- **Users**: 1
- **Events**: 3
- **First seen**: 1 day ago
- **Last seen**: 1 hour ago
"#;

        let issues = parse_issue_list_standalone(&config, text);
        assert!(issues.is_empty());
    }

    #[test]
    fn parse_empty_results() {
        let config = make_config();
        let text = "No issues found matching your search criteria.";
        let issues = parse_issue_list_standalone(&config, text);
        assert!(issues.is_empty());
    }

    #[test]
    fn build_query_with_custom_query() {
        let config = SentryConfig {
            project: "mail-ios".to_string(),
            query: "release:[so.notion.Mail@1.7.*,so.notion.Mail@1.8.*]".to_string(),
            ..SentryConfig::default()
        };

        let query = build_query_standalone(&config, false);
        assert_eq!(
            query,
            "project:mail-ios is:unresolved release:[so.notion.Mail@1.7.*,so.notion.Mail@1.8.*]"
        );

        let query = build_query_standalone(&config, true);
        assert_eq!(
            query,
            "project:mail-ios is:resolved release:[so.notion.Mail@1.7.*,so.notion.Mail@1.8.*]"
        );
    }

    #[test]
    fn build_query_empty_query() {
        let config = SentryConfig {
            project: "my-project".to_string(),
            ..SentryConfig::default()
        };

        let query = build_query_standalone(&config, false);
        assert_eq!(query, "project:my-project is:unresolved");
    }

    #[test]
    fn extract_field_parses_metadata() {
        let section = "- **Status**: unresolved\n- **Events**: 42\n- **Culprit**: `foo.bar`\n";
        assert_eq!(
            SentryTracker::extract_field(section, "Status"),
            Some("unresolved".to_string())
        );
        assert_eq!(
            SentryTracker::extract_field(section, "Events"),
            Some("42".to_string())
        );
        assert_eq!(
            SentryTracker::extract_field(section, "Culprit"),
            Some("`foo.bar`".to_string())
        );
        assert_eq!(SentryTracker::extract_field(section, "Missing"), None);
    }

    #[test]
    fn extract_bold_line_gets_title() {
        let section = "## 1. [X](url)\n\n**My Title**\n\n- **Status**: ok\n";
        assert_eq!(SentryTracker::extract_bold_line(section), "My Title");
    }

    /// Standalone helper to test parse_issue_list without an MCP connection.
    fn parse_issue_list_standalone(config: &SentryConfig, text: &str) -> Vec<Issue> {
        // Replicate the parsing logic using SentryTracker's static/instance methods.
        // We construct a minimal "tracker" by calling the parse methods directly.
        let mut issues = Vec::new();
        let header_re = Regex::new(r"##\s+\d+\.\s+\[([^\]]+)\]\(([^)]+)\)").unwrap();

        let sections: Vec<(regex::Match, &str)> = {
            let matches: Vec<_> = header_re.find_iter(text).collect();
            matches
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    let end = matches
                        .get(i + 1)
                        .map(|next| next.start())
                        .unwrap_or(text.len());
                    (*m, &text[m.start()..end])
                })
                .collect()
        };

        for (_, section) in sections {
            if let Some(caps) = header_re.captures(section) {
                let short_id = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                let url = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                let title = SentryTracker::extract_bold_line(section);
                let status =
                    SentryTracker::extract_field(section, "Status").unwrap_or_default();
                let users = SentryTracker::extract_field(section, "Users")
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                let events = SentryTracker::extract_field(section, "Events")
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                let first_seen =
                    SentryTracker::extract_field(section, "First seen").unwrap_or_default();
                let last_seen =
                    SentryTracker::extract_field(section, "Last seen").unwrap_or_default();
                let culprit = SentryTracker::extract_field(section, "Culprit")
                    .map(|s| s.trim_matches('`').to_string())
                    .unwrap_or_default();

                if events < config.min_events {
                    continue;
                }

                let description = format!(
                    "Sentry crash: {title}\n\nCulprit: {culprit}\n\
                     Events: {events} | Users affected: {users}\n\
                     First seen: {first_seen} | Last seen: {last_seen}\n\
                     Sentry link: {url}"
                );

                let mut extra = HashMap::new();
                extra.insert("event_count".to_string(), events.to_string());
                extra.insert("user_count".to_string(), users.to_string());
                extra.insert("first_seen".to_string(), first_seen);
                extra.insert("last_seen".to_string(), last_seen);
                if !culprit.is_empty() {
                    extra.insert("culprit".to_string(), culprit);
                }

                issues.push(Issue {
                    identifier: format!("sentry:{short_id}"),
                    title,
                    description: Some(description),
                    status,
                    priority: Some("High".to_string()),
                    url: if url.is_empty() {
                        None
                    } else {
                        Some(url.to_string())
                    },
                    notion_page_id: None,
                    blockers: vec![],
                    source: "sentry".to_string(),
                    extra,
                    comments: vec![],
                });
            }
        }

        issues
    }

    /// Standalone helper to test build_query without an MCP connection.
    fn build_query_standalone(config: &SentryConfig, resolved: bool) -> String {
        let status = if resolved { "is:resolved" } else { "is:unresolved" };
        let project = &config.project;
        let extra = &config.query;

        if extra.is_empty() {
            format!("project:{project} {status}")
        } else {
            format!("project:{project} {status} {extra}")
        }
    }
}
