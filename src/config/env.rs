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
}
