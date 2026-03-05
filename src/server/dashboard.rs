use crate::domain::state::StateSnapshot;

/// Render a simple HTML dashboard from the state snapshot.
pub fn render(snapshot: &StateSnapshot) -> String {
    let running_rows: String = snapshot.running.iter().map(|entry| {
        format!(
            "<tr><td>{}</td><td>{}</td><td>{:?}</td><td>{}</td></tr>",
            entry.issue.identifier,
            entry.issue.title,
            entry.session.status,
            entry.session.current_turn
        )
    }).collect();

    let completed_rows: String = snapshot.completed.iter().map(|entry| {
        format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            entry.issue_id,
            if entry.success { "Success" } else { "Failed" },
            entry.attempts,
            entry.completed_at.format("%Y-%m-%d %H:%M:%S")
        )
    }).collect();

    format!(r#"<!DOCTYPE html>
<html>
<head>
    <title>Symposium Dashboard</title>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <style>
        body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; margin: 2rem; background: #f5f5f5; }}
        h1 {{ color: #333; }}
        h2 {{ color: #555; margin-top: 2rem; }}
        table {{ border-collapse: collapse; width: 100%; background: white; border-radius: 8px; overflow: hidden; box-shadow: 0 1px 3px rgba(0,0,0,0.1); }}
        th, td {{ padding: 0.75rem 1rem; text-align: left; border-bottom: 1px solid #eee; }}
        th {{ background: #f8f9fa; font-weight: 600; color: #555; }}
        .stats {{ display: flex; gap: 1rem; margin: 1rem 0; }}
        .stat {{ background: white; padding: 1rem 1.5rem; border-radius: 8px; box-shadow: 0 1px 3px rgba(0,0,0,0.1); }}
        .stat-value {{ font-size: 2rem; font-weight: bold; color: #333; }}
        .stat-label {{ color: #888; font-size: 0.875rem; }}
        .empty {{ color: #999; padding: 2rem; text-align: center; }}
    </style>
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

    <script>setTimeout(() => location.reload(), 10000);</script>
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
            format!("<table><tr><th>Issue</th><th>Title</th><th>Status</th><th>Turns</th></tr>{running_rows}</table>")
        },
        completed_table = if snapshot.completed.is_empty() {
            "<div class=\"empty\">No completed sessions yet</div>".to_string()
        } else {
            format!("<table><tr><th>Issue</th><th>Result</th><th>Attempts</th><th>Completed</th></tr>{completed_rows}</table>")
        },
    )
}
