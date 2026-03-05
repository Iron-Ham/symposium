use super::mcp::McpClient;
use super::mcp_http::HttpMcpClient;
use super::TrackerClient;
use crate::config::schema::TrackerConfig;
use crate::domain::issue::Issue;
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

        let identifier = self.extract_property(props?, id_prop)?;
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
            extra,
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
}
