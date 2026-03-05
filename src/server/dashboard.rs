use crate::domain::session::AgentEventKind;
use crate::domain::state::StateSnapshot;

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

const STYLE: &str = r#"
    body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; margin: 2rem; background: #f5f5f5; }
    h1 { color: #333; }
    h1 a { color: #333; text-decoration: none; }
    h2 { color: #555; margin-top: 2rem; }
    table { border-collapse: collapse; width: 100%; background: white; border-radius: 8px; overflow: hidden; box-shadow: 0 1px 3px rgba(0,0,0,0.1); }
    th, td { padding: 0.75rem 1rem; text-align: left; border-bottom: 1px solid #eee; }
    th { background: #f8f9fa; font-weight: 600; color: #555; }
    a { color: #0066cc; text-decoration: none; }
    a:hover { text-decoration: underline; }
    .stats { display: flex; gap: 1rem; margin: 1rem 0; }
    .stat { background: white; padding: 1rem 1.5rem; border-radius: 8px; box-shadow: 0 1px 3px rgba(0,0,0,0.1); }
    .stat-value { font-size: 2rem; font-weight: bold; color: #333; }
    .stat-label { color: #888; font-size: 0.875rem; }
    .empty { color: #999; padding: 2rem; text-align: center; }
    .event { padding: 0.5rem 0; border-bottom: 1px solid #f0f0f0; font-size: 0.875rem; }
    .event:last-child { border-bottom: none; }
    .event-time { color: #999; font-family: monospace; margin-right: 0.75rem; }
    .event-status { color: #0066cc; font-weight: 600; }
    .event-tool { color: #7c3aed; }
    .event-result { color: #059669; }
    .event-error { color: #dc2626; font-weight: 600; }
    .event-turn { color: #d97706; font-weight: 600; }
    .event-text { color: #555; }
    .event-detail { color: #888; font-family: monospace; font-size: 0.8rem; display: block; margin-top: 0.25rem; white-space: pre-wrap; word-break: break-all; max-height: 4rem; overflow: hidden; }
    .meta { background: white; padding: 1rem 1.5rem; border-radius: 8px; box-shadow: 0 1px 3px rgba(0,0,0,0.1); margin: 1rem 0; }
    .meta-row { display: flex; gap: 2rem; margin: 0.25rem 0; }
    .meta-label { color: #888; min-width: 6rem; }
    .events-container { background: white; border-radius: 8px; box-shadow: 0 1px 3px rgba(0,0,0,0.1); padding: 1rem 1.5rem; max-height: 70vh; overflow-y: auto; }
"#;

/// Render the main dashboard.
pub fn render(snapshot: &StateSnapshot) -> String {
    let running_rows: String = snapshot
        .running
        .iter()
        .map(|entry| {
            let last_event = entry
                .session
                .events
                .last()
                .map(|e| match &e.kind {
                    AgentEventKind::Status { status } => status.clone(),
                    AgentEventKind::ToolCall { name, .. } => format!("→ {name}"),
                    AgentEventKind::ToolResult { name, .. } => format!("← {name}"),
                    AgentEventKind::TurnComplete { turn } => format!("Turn {turn} done"),
                    AgentEventKind::Error { message } => format!("Error: {message}"),
                    AgentEventKind::Text { .. } => "Thinking...".into(),
                })
                .unwrap_or_default();
            format!(
                "<tr><td><a href=\"/issue/{}\">{}</a></td><td>{}</td><td>{:?}</td><td>{}</td></tr>",
                entry.issue.identifier,
                entry.issue.identifier,
                html_escape(&entry.issue.title),
                entry.session.status,
                html_escape(&last_event),
            )
        })
        .collect();

    let completed_rows: String = snapshot
        .completed
        .iter()
        .map(|entry| {
            format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                entry.issue_id,
                if entry.success { "✓ Success" } else { "✗ Failed" },
                entry.attempts,
                entry.completed_at.format("%H:%M:%S")
            )
        })
        .collect();

    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>Symposium</title>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <style>{STYLE}</style>
</head>
<body>
    <h1>Symposium</h1>

    <div class="stats">
        <div class="stat">
            <div class="stat-value">{running}</div>
            <div class="stat-label">Running</div>
        </div>
        <div class="stat">
            <div class="stat-value">{retries}</div>
            <div class="stat-label">Pending Retries</div>
        </div>
        <div class="stat">
            <div class="stat-value">{completed}</div>
            <div class="stat-label">Completed</div>
        </div>
        <div class="stat">
            <div class="stat-value">{tokens_in} / {tokens_out}</div>
            <div class="stat-label">Tokens (In / Out)</div>
        </div>
    </div>

    <h2>Running Sessions</h2>
    {running_table}

    <h2>Recently Completed</h2>
    {completed_table}

    <script>setTimeout(() => location.reload(), 5000);</script>
</body>
</html>"#,
        running = snapshot.running.len(),
        retries = snapshot.retries.len(),
        completed = snapshot.completed.len(),
        tokens_in = snapshot.tokens.input_tokens,
        tokens_out = snapshot.tokens.output_tokens,
        running_table = if snapshot.running.is_empty() {
            "<div class=\"empty\">No running sessions</div>".to_string()
        } else {
            format!("<table><tr><th>Issue</th><th>Title</th><th>Status</th><th>Latest</th></tr>{running_rows}</table>")
        },
        completed_table = if snapshot.completed.is_empty() {
            "<div class=\"empty\">No completed sessions yet</div>".to_string()
        } else {
            format!("<table><tr><th>Issue</th><th>Result</th><th>Attempts</th><th>Completed</th></tr>{completed_rows}</table>")
        },
    )
}

/// Render a detail page for a specific issue.
pub fn render_issue_detail(snapshot: &StateSnapshot, issue_id: &str) -> String {
    let entry = snapshot.running.iter().find(|e| e.issue.identifier == issue_id);

    let Some(entry) = entry else {
        return format!(
            r#"<!DOCTYPE html>
<html><head><title>Not Found</title><meta charset="utf-8"><style>{STYLE}</style></head>
<body><h1><a href="/">← Symposium</a></h1><div class="empty">Issue {issue_id} not found in running sessions</div></body></html>"#,
        );
    };

    let events_html: String = entry
        .session
        .events
        .iter()
        .map(|event| {
            let time = event.timestamp.format("%H:%M:%S");
            match &event.kind {
                AgentEventKind::Status { status } => {
                    format!(
                        "<div class=\"event\"><span class=\"event-time\">{time}</span><span class=\"event-status\">{}</span></div>",
                        html_escape(status)
                    )
                }
                AgentEventKind::ToolCall { name, arguments } => {
                    format!(
                        "<div class=\"event\"><span class=\"event-time\">{time}</span><span class=\"event-tool\">→ {}</span><span class=\"event-detail\">{}</span></div>",
                        html_escape(name),
                        html_escape(arguments)
                    )
                }
                AgentEventKind::ToolResult { name, truncated } => {
                    format!(
                        "<div class=\"event\"><span class=\"event-time\">{time}</span><span class=\"event-result\">← {}</span><span class=\"event-detail\">{}</span></div>",
                        html_escape(name),
                        html_escape(truncated)
                    )
                }
                AgentEventKind::TurnComplete { turn } => {
                    format!(
                        "<div class=\"event\"><span class=\"event-time\">{time}</span><span class=\"event-turn\">Turn {turn} complete</span></div>"
                    )
                }
                AgentEventKind::Error { message } => {
                    format!(
                        "<div class=\"event\"><span class=\"event-time\">{time}</span><span class=\"event-error\">Error: {}</span></div>",
                        html_escape(message)
                    )
                }
                AgentEventKind::Text { text } => {
                    let preview: String = text.chars().take(200).collect();
                    format!(
                        "<div class=\"event\"><span class=\"event-time\">{time}</span><span class=\"event-text\">{}</span></div>",
                        html_escape(&preview)
                    )
                }
            }
        })
        .collect();

    let desc = entry
        .issue
        .description
        .as_deref()
        .unwrap_or("No description");

    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>Issue {id} - Symposium</title>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <style>{STYLE}</style>
</head>
<body>
    <h1><a href="/">← Symposium</a></h1>
    <h2>{id}: {title}</h2>

    <div class="meta">
        <div class="meta-row"><span class="meta-label">Status</span><span>{status:?}</span></div>
        <div class="meta-row"><span class="meta-label">Priority</span><span>{priority}</span></div>
        <div class="meta-row"><span class="meta-label">Started</span><span>{started}</span></div>
        <div class="meta-row"><span class="meta-label">Last Activity</span><span>{last_activity}</span></div>
        <div class="meta-row"><span class="meta-label">Description</span><span>{description}</span></div>
    </div>

    <h2>Agent Activity</h2>
    <div class="events-container">
        {events}
    </div>

    <script>setTimeout(() => location.reload(), 3000);</script>
</body>
</html>"#,
        id = entry.issue.identifier,
        title = html_escape(&entry.issue.title),
        status = entry.session.status,
        priority = html_escape(entry.issue.priority.as_deref().unwrap_or("—")),
        started = entry.session.started_at.format("%H:%M:%S"),
        last_activity = entry.session.last_activity.format("%H:%M:%S"),
        description = html_escape(desc),
        events = if events_html.is_empty() {
            "<div class=\"empty\">No events yet</div>".to_string()
        } else {
            events_html
        },
    )
}
