use regex::Regex;
use std::env;

use super::schema::ServiceConfig;

/// Expand `$VAR`, `${VAR}`, and leading `~` in a string.
pub fn expand_vars(input: &str) -> String {
    // Expand ~ at start of string to $HOME
    let expanded = if input.starts_with("~/") {
        match env::var("HOME") {
            Ok(home) => format!("{}{}", home, &input[1..]),
            Err(_) => input.to_string(),
        }
    } else {
        input.to_string()
    };

    // Expand ${VAR} and $VAR
    let re = Regex::new(r"\$\{([^}]+)\}|\$([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    re.replace_all(&expanded, |caps: &regex::Captures| {
        let var_name = caps
            .get(1)
            .or_else(|| caps.get(2))
            .map(|m| m.as_str())
            .unwrap_or("");
        env::var(var_name).unwrap_or_default()
    })
    .into_owned()
}

/// Apply env expansion to key string fields in a ServiceConfig.
pub fn expand_config(config: &mut ServiceConfig) {
    config.tracker.mcp_command = expand_vars(&config.tracker.mcp_command);
    if let Some(ref s) = config.tracker.mcp_url {
        config.tracker.mcp_url = Some(expand_vars(s));
    }
    config.tracker.database_id = expand_vars(&config.tracker.database_id);
    config.workspace.root = expand_vars(&config.workspace.root);
    config.codex.command = expand_vars(&config.codex.command);

    if let Some(ref s) = config.hooks.after_create {
        config.hooks.after_create = Some(expand_vars(s));
    }
    if let Some(ref s) = config.hooks.before_run {
        config.hooks.before_run = Some(expand_vars(s));
    }
    if let Some(ref s) = config.hooks.after_run {
        config.hooks.after_run = Some(expand_vars(s));
    }
    if let Some(ref s) = config.hooks.before_remove {
        config.hooks.before_remove = Some(expand_vars(s));
    }

    config.sentry.org = expand_vars(&config.sentry.org);
    config.sentry.project = expand_vars(&config.sentry.project);
    config.sentry.mcp_url = expand_vars(&config.sentry.mcp_url);
    config.sentry.query = expand_vars(&config.sentry.query);

    for server in config.mcp_servers.values_mut() {
        if let Some(ref s) = server.url {
            server.url = Some(expand_vars(s));
        }
        if let Some(ref s) = server.command {
            server.command = Some(expand_vars(s));
        }
        if let Some(ref args) = server.args {
            server.args = Some(args.iter().map(|a| expand_vars(a)).collect());
        }
        if let Some(ref env_map) = server.env {
            server.env = Some(
                env_map
                    .iter()
                    .map(|(k, v)| (k.clone(), expand_vars(v)))
                    .collect(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_env_var() {
        // SAFETY: test is single-threaded
        unsafe { env::set_var("SYMPOSIUM_TEST_VAR", "hello") };
        assert_eq!(expand_vars("$SYMPOSIUM_TEST_VAR"), "hello");
        assert_eq!(expand_vars("${SYMPOSIUM_TEST_VAR}"), "hello");
        assert_eq!(
            expand_vars("prefix-$SYMPOSIUM_TEST_VAR-suffix"),
            "prefix-hello-suffix"
        );
        unsafe { env::remove_var("SYMPOSIUM_TEST_VAR") };
    }

    #[test]
    fn expand_tilde() {
        let home = env::var("HOME").unwrap_or_default();
        assert_eq!(expand_vars("~/foo/bar"), format!("{}/foo/bar", home));
    }

    #[test]
    fn expand_missing_var() {
        assert_eq!(expand_vars("$SYMPOSIUM_NONEXISTENT_VAR_XYZ"), "");
    }

    #[test]
    fn expand_mcp_server_env_vars() {
        use crate::config::schema::McpServerConfig;
        use std::collections::HashMap;

        // SAFETY: test is single-threaded
        unsafe { env::set_var("SYMPOSIUM_MCP_KEY", "expanded-secret") };

        let mut config = ServiceConfig::default();
        config.mcp_servers.insert(
            "test".to_string(),
            McpServerConfig {
                server_type: "http".to_string(),
                url: Some("https://$SYMPOSIUM_MCP_KEY.example.com".to_string()),
                command: Some("$SYMPOSIUM_MCP_KEY".to_string()),
                args: Some(vec!["--token=$SYMPOSIUM_MCP_KEY".to_string()]),
                env: Some(HashMap::from([(
                    "API_KEY".to_string(),
                    "$SYMPOSIUM_MCP_KEY".to_string(),
                )])),
            },
        );

        expand_config(&mut config);

        let server = &config.mcp_servers["test"];
        assert_eq!(
            server.url.as_deref(),
            Some("https://expanded-secret.example.com")
        );
        assert_eq!(server.command.as_deref(), Some("expanded-secret"));
        assert_eq!(
            server.args.as_deref(),
            Some(["--token=expanded-secret".to_string()].as_slice())
        );
        assert_eq!(
            server.env.as_ref().unwrap().get("API_KEY").unwrap(),
            "expanded-secret"
        );

        unsafe { env::remove_var("SYMPOSIUM_MCP_KEY") };
    }
}
