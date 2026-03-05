use super::mcp::McpClient;
use super::TrackerClient;
use crate::config::schema::TrackerConfig;
use crate::domain::issue::Issue;
use crate::error::{Error, Result};
use serde_json::Value;

pub struct NotionTracker {
    client: McpClient,
    config: TrackerConfig,
    data_source_id: Option<String>,
}

impl NotionTracker {
    pub async fn new(config: TrackerConfig) -> Result<Self> {
        let parts: Vec<&str> = config.mcp_command.split_whitespace().collect();
        let (cmd, args) = parts
            .split_first()
            .ok_or_else(|| Error::Tracker("empty mcp_command".into()))?;
        let client = McpClient::new(cmd, args).await?;
        Ok(Self {
            client,
            config,
            data_source_id: None,
        })
    }

    /// Discover the data source ID for the configured database.
    async fn ensure_data_source(&mut self) -> Result<String> {
        if let Some(ref id) = self.data_source_id {
            return Ok(id.clone());
        }
        // Query data sources to find our database
        let _result = self
            .client
            .call_tool("notion-query-data-sources", serde_json::json!({"query": ""}))
            .await?;
        let ds_id = format!("collection://{}", self.config.database_id);
        self.data_source_id = Some(ds_id.clone());
        Ok(ds_id)
    }

    /// Build SQL query for fetching issues with given statuses.
    fn build_status_query(&self, ds_id: &str, statuses: &[String]) -> String {
        let status_list = statuses
            .iter()
            .map(|s| format!("'{s}'"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "SELECT * FROM \"{ds_id}\" WHERE \"{}\" IN ({status_list})",
            self.config.property_status
        )
    }

    /// Extract Issue structs from a Notion query result.
    fn extract_issues(&self, result: &Value) -> Vec<Issue> {
        let mut issues = Vec::new();

        let rows = result
            .get("results")
            .or_else(|| result.get("content"))
            .and_then(|v| v.as_array());

        let Some(rows) = rows else {
            return issues;
        };

        for row in rows {
            if let Some(issue) = self.parse_issue_row(row) {
                issues.push(issue);
            }
        }
        issues
    }

    fn parse_issue_row(&self, row: &Value) -> Option<Issue> {
        let props = row.get("properties").or(Some(row));

        let identifier = self.extract_property(props?, &self.config.property_id)?;
        let title = self
            .extract_property(props?, "Name")
            .or_else(|| self.extract_property(props?, "Title"))
            .unwrap_or_default();
        let status = self
            .extract_property(props?, &self.config.property_status)
            .unwrap_or_default();
        let priority = self.extract_property(props?, &self.config.property_priority);
        let page_id = row.get("id").and_then(|v| v.as_str()).map(String::from);
        let url = row.get("url").and_then(|v| v.as_str()).map(String::from);

        Some(Issue {
            identifier,
            title,
            description: None,
            status,
            priority,
            url,
            notion_page_id: page_id,
            blockers: vec![],
        })
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
        let ds_id = self.ensure_data_source().await?;
        let sql = self.build_status_query(&ds_id, &self.config.active_states);
        let result = self
            .client
            .call_tool(
                "notion-query-data-sources",
                serde_json::json!({"query": sql}),
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
        let ds_id = self.ensure_data_source().await?;
        let sql = self.build_status_query(&ds_id, &self.config.terminal_states);
        let result = self
            .client
            .call_tool(
                "notion-query-data-sources",
                serde_json::json!({"query": sql}),
            )
            .await?;
        Ok(self.extract_issues(&result))
    }

    async fn agent_query(&mut self, sql: &str) -> Result<Value> {
        self.client
            .call_tool(
                "notion-query-data-sources",
                serde_json::json!({"query": sql}),
            )
            .await
    }
}
