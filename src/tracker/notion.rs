use super::mcp::McpClient;
use super::mcp_http::HttpMcpClient;
use super::TrackerClient;
use crate::config::schema::TrackerConfig;
use crate::domain::issue::{Comment, Issue};
use crate::error::{Error, Result};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// Wraps either a stdio or HTTP MCP client.
enum McpTransport {
    Stdio(McpClient),
    Http(HttpMcpClient),
}

impl McpTransport {
    async fn call_tool(&mut self, name: &str, args: Value) -> Result<Value> {
        match self {
            Self::Stdio(c) => c.call_tool(name, args).await,
            Self::Http(c) => c.call_tool(name, args).await,
        }
    }
}

pub struct NotionTracker {
    client: McpTransport,
    config: TrackerConfig,
    data_source_id: Option<String>,
}

impl NotionTracker {
    pub async fn new(config: TrackerConfig) -> Result<Self> {
        let client = if let Some(ref url) = config.mcp_url {
            tracing::info!(url, "connecting to MCP via HTTP");
            McpTransport::Http(HttpMcpClient::new(url).await?)
        } else {
            let parts: Vec<&str> = config.mcp_command.split_whitespace().collect();
            let (cmd, args) = parts
                .split_first()
                .ok_or_else(|| Error::Tracker("empty mcp_command".into()))?;
            McpTransport::Stdio(McpClient::new(cmd, args).await?)
        };
        Ok(Self {
            client,
            config,
            data_source_id: None,
        })
    }

    /// Get the data source URL for the configured database.
    fn data_source_url(&mut self) -> String {
        if let Some(ref id) = self.data_source_id {
            return id.clone();
        }
        let ds_id = format!("collection://{}", self.config.database_id);
        self.data_source_id = Some(ds_id.clone());
        ds_id
    }

    /// Build SQL query for fetching issues with given statuses.
    fn build_status_query(&self, ds_id: &str, statuses: &[String]) -> String {
        let status_list = statuses
            .iter()
            .map(|s| format!("'{s}'"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut query = format!(
            "SELECT * FROM \"{ds_id}\" WHERE \"{}\" IN ({status_list})",
            self.config.property_status
        );
        if let Some(ref user_id) = self.config.assignee_user_id {
            query.push_str(&format!(
                " AND \"{}\" LIKE '%{user_id}%'",
                self.config.property_assignee
            ));
        }
        if let Some(ref prop) = self.config.skip_if_set {
            query.push_str(&format!(" AND \"{prop}\" IS NULL"));
        }
        query
    }

    /// Unwrap the MCP tool response to get the inner data.
    /// MCP tools return `{"content": [{"type":"text","text":"{...}"}]}`.
    fn unwrap_tool_result(result: &Value) -> Value {
        // Try to extract text from MCP content blocks
        if let Some(content) = result.get("content").and_then(|v| v.as_array()) {
            for block in content {
                if let Some(text) = block.get("text").and_then(|v| v.as_str())
                    && let Ok(parsed) = serde_json::from_str::<Value>(text)
                {
                    return parsed;
                }
            }
        }
        // Already unwrapped or direct format
        result.clone()
    }

    /// Extract Issue structs from a Notion query result.
    fn extract_issues(&self, result: &Value) -> Vec<Issue> {
        let data = Self::unwrap_tool_result(result);
        let mut issues = Vec::new();

        let rows = data
            .get("results")
            .and_then(|v| v.as_array());

        let Some(rows) = rows else {
            tracing::debug!(response = %data, "no results array in response");
            return issues;
        };

        for row in rows {
            if let Some(issue) = self.parse_issue_row(row) {
                issues.push(issue);
            }
        }

        tracing::info!(count = issues.len(), "fetched issues from Notion");
        issues
    }

    fn parse_issue_row(&self, row: &Value) -> Option<Issue> {
        let props = row.get("properties").or(Some(row));

        let id_prop = self.config.property_id.as_str();
        let title_prop = self.config.property_title.as_str();
        let status_prop = self.config.property_status.as_str();
        let priority_prop = self.config.property_priority.as_str();
        let desc_prop = self.config.property_description.as_str();

        let raw_id = self.extract_property(props?, id_prop)?;
        let identifier = match &self.config.id_prefix {
            Some(prefix) => format!("{prefix}{raw_id}"),
            None => raw_id,
        };
        let title = self.extract_property(props?, title_prop).unwrap_or_default();
        let status = self
            .extract_property(props?, status_prop)
            .unwrap_or_default();
        let priority = self.extract_property(props?, priority_prop);
        let description = self.extract_property(props?, desc_prop);
        let page_id = row.get("id").and_then(|v| v.as_str()).map(String::from);
        let url = row.get("url").and_then(|v| v.as_str()).map(String::from);

        // Collect extra properties not already mapped to known fields
        let known: HashSet<&str> =
            [id_prop, title_prop, status_prop, priority_prop, desc_prop].into();
        let extra = self.extract_extra_properties(props?, &known);

        Some(Issue {
            identifier,
            title,
            description,
            status,
            priority,
            url,
            notion_page_id: page_id,
            blockers: vec![],
            source: "notion".to_string(),
            extra,
            comments: vec![],
            workflow_id: String::new(),
        })
    }

    fn extract_extra_properties(
        &self,
        props: &Value,
        known: &HashSet<&str>,
    ) -> HashMap<String, String> {
        let mut extra = HashMap::new();
        if let Some(obj) = props.as_object() {
            for (key, _) in obj {
                if known.contains(key.as_str()) {
                    continue;
                }
                if let Some(val) = self.extract_property(props, key) {
                    // Use lowercase key so templates can use {{ issue.platform }}
                    extra.insert(key.to_lowercase(), val);
                }
            }
        }
        extra
    }

    fn extract_property(&self, props: &Value, name: &str) -> Option<String> {
        let prop = props.get(name)?;

        // Direct string
        if let Some(s) = prop.as_str() {
            return Some(s.to_string());
        }

        // Rich text: {"rich_text": [{"plain_text": "..."}]}
        if let Some(rt) = prop.get("rich_text").and_then(|v| v.as_array()) {
            let text: String = rt
                .iter()
                .filter_map(|t| t.get("plain_text").and_then(|v| v.as_str()))
                .collect();
            if !text.is_empty() {
                return Some(text);
            }
        }

        // Title: {"title": [{"plain_text": "..."}]}
        if let Some(t) = prop.get("title").and_then(|v| v.as_array()) {
            let text: String = t
                .iter()
                .filter_map(|t| t.get("plain_text").and_then(|v| v.as_str()))
                .collect();
            if !text.is_empty() {
                return Some(text);
            }
        }

        // Select: {"select": {"name": "..."}}
        if let Some(s) = prop
            .get("select")
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str())
        {
            return Some(s.to_string());
        }

        // Status: {"status": {"name": "..."}}
        if let Some(s) = prop
            .get("status")
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str())
        {
            return Some(s.to_string());
        }

        // Number
        if let Some(n) = prop.as_f64() {
            return Some(n.to_string());
        }

        None
    }
}

impl NotionTracker {
    /// Extract the raw text from an MCP tool response content block.
    fn extract_text(result: &Value) -> Option<String> {
        // MCP returns {"content": [{"type":"text","text":"..."}]}
        // or the tool result may already have a "text" field at the top level.
        if let Some(text) = result.get("text").and_then(|v| v.as_str()) {
            return Some(text.to_string());
        }
        let content = result.get("content").and_then(|v| v.as_array())?;
        for block in content {
            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                return Some(text.to_string());
            }
        }
        None
    }

    /// Parse comments from the Notion MCP `notion-get-comments` XML response.
    ///
    /// The response is XML with structure:
    /// ```xml
    /// <discussions>
    ///   <discussion ...>
    ///     <comment user-url="user://uuid" datetime="...">body text</comment>
    ///     ...
    ///   </discussion>
    /// </discussions>
    /// ```
    fn parse_comments(result: &Value) -> Vec<Comment> {
        let Some(xml) = Self::extract_text(result) else {
            tracing::debug!("no text content in comments response");
            return vec![];
        };
        Self::parse_comments_xml(&xml)
    }

    fn parse_comments_xml(xml: &str) -> Vec<Comment> {
        use regex::Regex;

        // Match <comment ...>...</comment> elements. The body can contain
        // inline XML tags like <mention-user/>, <br/>, <mention-date/>.
        let comment_re = Regex::new(
            r#"<comment\b([^>]*)>([\s\S]*?)</comment>"#
        ).expect("valid regex");

        let datetime_re = Regex::new(
            r#"datetime="([^"]*)""#
        ).expect("valid regex");

        let user_url_re = Regex::new(
            r#"user-url="user://([^"]*)""#
        ).expect("valid regex");

        // Strip inline XML tags to get plain text body
        let tag_re = Regex::new(r"<[^>]+/?>").expect("valid regex");

        let mut comments = Vec::new();

        for cap in comment_re.captures_iter(xml) {
            let attrs = &cap[1];
            let raw_body = &cap[2];

            // Extract datetime
            let created_at = datetime_re
                .captures(attrs)
                .map(|c| c[1].to_string());

            // Extract user URL as author identifier (we don't have names, only UUIDs)
            let author = user_url_re
                .captures(attrs)
                .map(|c| c[1].to_string())
                .unwrap_or_else(|| "Unknown".to_string());

            // Clean body: strip XML tags, normalize <br/> to newlines
            let body = raw_body.replace("<br/>", "\n");
            let body = tag_re.replace_all(&body, "").trim().to_string();

            if !body.is_empty() {
                comments.push(Comment {
                    author,
                    body,
                    created_at,
                });
            }
        }

        comments
    }
}

impl TrackerClient for NotionTracker {
    async fn fetch_candidate_issues(&mut self) -> Result<Vec<Issue>> {
        let ds_url = self.data_source_url();
        let sql = self.build_status_query(&ds_url, &self.config.active_states);
        let result = self
            .client
            .call_tool(
                "notion-query-data-sources",
                serde_json::json!({
                    "data": {
                        "data_source_urls": [&ds_url],
                        "query": sql
                    }
                }),
            )
            .await?;
        Ok(self.extract_issues(&result))
    }

    async fn fetch_issue_states_by_ids(&mut self, ids: &[String]) -> Result<Vec<Issue>> {
        let mut issues = Vec::new();
        for id in ids {
            match self
                .client
                .call_tool("notion-fetch", serde_json::json!({"url": id}))
                .await
            {
                Ok(result) => {
                    if let Some(issue) = self.parse_issue_row(&result) {
                        issues.push(issue);
                    }
                }
                Err(e) => {
                    tracing::warn!(id, "failed to fetch issue state: {e}");
                }
            }
        }
        Ok(issues)
    }

    async fn fetch_terminal_issues(&mut self) -> Result<Vec<Issue>> {
        let ds_url = self.data_source_url();
        let sql = self.build_status_query(&ds_url, &self.config.terminal_states);
        let result = self
            .client
            .call_tool(
                "notion-query-data-sources",
                serde_json::json!({
                    "data": {
                        "data_source_urls": [&ds_url],
                        "query": sql
                    }
                }),
            )
            .await?;
        Ok(self.extract_issues(&result))
    }

    async fn agent_query(&mut self, sql: &str) -> Result<Value> {
        let ds_url = self.data_source_url();
        self.client
            .call_tool(
                "notion-query-data-sources",
                serde_json::json!({
                    "data": {
                        "data_source_urls": [&ds_url],
                        "query": sql
                    }
                }),
            )
            .await
    }

    async fn fetch_comments(&mut self, page_id: &str) -> Result<Vec<Comment>> {
        let result = self
            .client
            .call_tool(
                "notion-get-comments",
                serde_json::json!({ "page_id": page_id }),
            )
            .await?;
        let comments = Self::parse_comments(&result);
        tracing::debug!(page_id, count = comments.len(), "fetched comments");
        Ok(comments)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_comments_xml_basic() {
        let xml = r#"<discussions total-count="1" shown-count="1">
<discussion id="disc-1" comment-count="2" resolved="false" type="comment" context="page">
<comment id="c1" user-url="user://alice-uuid" datetime="2026-03-01T10:00:00.000Z">This happens on login</comment>
<comment id="c2" user-url="user://bob-uuid" datetime="2026-03-02T14:30:00.000Z">Confirmed on staging</comment>
</discussion>
</discussions>"#;

        let comments = NotionTracker::parse_comments_xml(xml);
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].author, "alice-uuid");
        assert_eq!(comments[0].body, "This happens on login");
        assert_eq!(
            comments[0].created_at.as_deref(),
            Some("2026-03-01T10:00:00.000Z")
        );
        assert_eq!(comments[1].author, "bob-uuid");
        assert_eq!(comments[1].body, "Confirmed on staging");
    }

    #[test]
    fn parse_comments_from_mcp_content_wrapper() {
        let xml = r#"<discussions><discussion><comment id="c1" user-url="user://carol-uuid" datetime="2026-03-03T09:00:00.000Z">Fix the auth check</comment></discussion></discussions>"#;
        let data = serde_json::json!({
            "content": [{
                "type": "text",
                "text": xml
            }]
        });

        let comments = NotionTracker::parse_comments(&data);
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].author, "carol-uuid");
        assert_eq!(comments[0].body, "Fix the auth check");
    }

    #[test]
    fn parse_comments_strips_inline_tags() {
        let xml = r#"<discussions><discussion>
<comment id="c1" user-url="user://uuid1" datetime="2026-01-05T19:25:00.000Z"><mention-user url="user://other"/> you should fix this<br/>See the logs</comment>
</discussion></discussions>"#;

        let comments = NotionTracker::parse_comments_xml(xml);
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].body, "you should fix this\nSee the logs");
    }

    #[test]
    fn parse_comments_skips_empty_body() {
        let xml = r#"<discussions><discussion>
<comment id="c1" user-url="user://uuid1" datetime="2026-01-01T00:00:00Z"><mention-user url="user://x"/></comment>
<comment id="c2" user-url="user://uuid2" datetime="2026-01-02T00:00:00Z">Real comment</comment>
</discussion></discussions>"#;

        let comments = NotionTracker::parse_comments_xml(xml);
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].body, "Real comment");
    }

    #[test]
    fn parse_comments_missing_user_url_defaults() {
        let xml = r#"<discussions><discussion>
<comment id="c1" datetime="2026-01-01T00:00:00Z">Anonymous comment</comment>
</discussion></discussions>"#;

        let comments = NotionTracker::parse_comments_xml(xml);
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].author, "Unknown");
    }

    #[test]
    fn parse_comments_empty_discussions() {
        let xml = r#"<discussions total-count="0" shown-count="0"></discussions>"#;

        let comments = NotionTracker::parse_comments_xml(xml);
        assert!(comments.is_empty());
    }

    #[test]
    fn parse_comments_from_text_field() {
        let xml = r#"<discussions><discussion><comment id="c1" user-url="user://uuid1" datetime="2026-01-01T00:00:00Z">Direct text</comment></discussion></discussions>"#;
        let data = serde_json::json!({ "text": xml });

        let comments = NotionTracker::parse_comments(&data);
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].body, "Direct text");
    }
}
