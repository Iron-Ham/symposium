//! Probe a few argument shapes for the Sentry MCP `search_issues` tool to find
//! one that reliably scopes to a single Sentry project.
//!
//! Run with:
//!   cargo run --example sentry_probe
//!
//! Reads OAuth state from the same cache directory as the orchestrator, so make
//! sure you've authorized the Sentry MCP at least once via a normal orchestrator
//! run before invoking this.

use serde_json::{Value, json};
use std::collections::BTreeMap;
use symposium::tracker::mcp_http::HttpMcpClient;

const SENTRY_MCP_URL: &str = "https://mcp.sentry.dev/mcp";
const ORG: &str = "notion";
const PROJECT: &str = "mail-ios";
// Match the existing config — keep filters identical so we're comparing the
// scoping behavior, not the filter set.
const EXTRA: &str = "error.unhandled:true";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let mut client = HttpMcpClient::new(SENTRY_MCP_URL).await?;

    // --- 1) Dump the search_issues schema so we know what args the server actually accepts.
    println!("\n=== search_issues schema ===");
    let tools = client.list_tools().await?;
    if let Some(arr) = tools.get("tools").and_then(|v| v.as_array())
        && let Some(t) = arr.iter().find(|t| {
            t.get("name").and_then(|n| n.as_str()) == Some("search_issues")
        })
    {
        if let Some(schema) = t.get("inputSchema") {
            println!("{}", serde_json::to_string_pretty(schema)?);
        } else {
            println!("(no inputSchema field)");
        }
    } else {
        println!("(search_issues tool not found)");
    }

    // --- 2) Run several call shapes and summarize what comes back.
    let nl_with_project = format!("project:{PROJECT} is:unresolved {EXTRA}");
    let nl_without_project = format!("is:unresolved {EXTRA}");

    let variants: Vec<(&str, Value)> = vec![
        (
            "A) baseline: NL only, project encoded inside naturalLanguageQuery",
            json!({
                "organizationSlug": ORG,
                "naturalLanguageQuery": nl_with_project,
            }),
        ),
        (
            "B) NL + structured projectSlug",
            json!({
                "organizationSlug": ORG,
                "projectSlug": PROJECT,
                "naturalLanguageQuery": nl_without_project,
            }),
        ),
        (
            "C) NL + structured projectSlugs (array)",
            json!({
                "organizationSlug": ORG,
                "projectSlugs": [PROJECT],
                "naturalLanguageQuery": nl_without_project,
            }),
        ),
        (
            "D) literal `query` arg (bypass the NLU)",
            json!({
                "organizationSlug": ORG,
                "query": nl_with_project,
            }),
        ),
        (
            "E) literal `query` + structured projectSlug",
            json!({
                "organizationSlug": ORG,
                "projectSlug": PROJECT,
                "query": format!("is:unresolved {EXTRA}"),
            }),
        ),
        (
            "F) SCHEMA-CORRECT: projectSlugOrId + query, limit 100",
            json!({
                "organizationSlug": ORG,
                "projectSlugOrId": PROJECT,
                "query": format!("is:unresolved {EXTRA}"),
                "limit": 100,
            }),
        ),
        (
            "G) SCHEMA-CORRECT, no extra filter (sanity: full mail-ios unresolved)",
            json!({
                "organizationSlug": ORG,
                "projectSlugOrId": PROJECT,
                "query": "is:unresolved",
                "limit": 100,
            }),
        ),
    ];

    for (label, args) in variants {
        println!("\n=== {label} ===");
        println!(
            "args: {}",
            serde_json::to_string(&args).unwrap_or_default()
        );
        match client.call_tool("search_issues", args).await {
            Ok(result) => summarize(&result),
            Err(e) => println!("ERROR: {e}"),
        }
    }

    Ok(())
}

/// Summarize a tool response: error flag, total issue count, and the breakdown
/// of short-ID prefixes so we can see whether `mail-web` etc. leaked through.
fn summarize(result: &Value) {
    let is_error = result
        .get("isError")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if is_error {
        println!("isError=true");
    }

    let text = extract_text(result);

    // Sentry MCP responses sometimes include a "Found **N** issues:" line.
    let found_line = text
        .lines()
        .find(|l| l.contains("Found") && l.contains("issues"))
        .unwrap_or("(no 'Found N issues' line)");
    println!("header: {}", found_line.trim());

    // Pull every short-id from "## N. [SHORT-ID](url)" headers.
    let re = regex::Regex::new(r"##\s+\d+\.\s+\[([A-Z0-9-]+)\]").unwrap();
    let mut prefixes: BTreeMap<String, usize> = BTreeMap::new();
    let mut total = 0usize;
    for caps in re.captures_iter(&text) {
        total += 1;
        let id = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        // Prefix = everything up to the last "-".
        let prefix = id.rsplit_once('-').map(|(p, _)| p).unwrap_or(id);
        *prefixes.entry(prefix.to_string()).or_insert(0) += 1;
    }
    println!("parsed total: {total}");
    if prefixes.is_empty() {
        println!("prefixes: (none parsed — first 400 chars of body follows)");
        let preview: String = text.chars().take(400).collect();
        println!("--- body preview ---\n{preview}\n--- end preview ---");
    } else {
        println!("prefixes:");
        for (p, n) in prefixes {
            println!("  {p:<20} {n}");
        }
    }
}

fn extract_text(result: &Value) -> String {
    if let Some(content) = result.get("content").and_then(|v| v.as_array()) {
        return content
            .iter()
            .filter_map(|b| b.get("text").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
            .join("\n");
    }
    result
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}
