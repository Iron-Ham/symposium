use crate::error::{Error, Result};
use std::path::Path;

/// Sanitize an issue key for use as a directory name.
/// Keeps alphanumeric, hyphens, underscores. Replaces everything else with '-'.
/// Collapses consecutive hyphens. Trims leading/trailing hyphens.
pub fn sanitize_key(key: &str) -> String {
    let replaced: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();

    // Collapse consecutive hyphens
    let mut result = String::with_capacity(replaced.len());
    let mut prev_hyphen = false;
    for c in replaced.chars() {
        if c == '-' {
            if !prev_hyphen {
                result.push(c);
            }
            prev_hyphen = true;
        } else {
            prev_hyphen = false;
            result.push(c);
        }
    }

    result.trim_matches('-').to_string()
}

/// Verify that `child` is contained within `parent` directory.
/// Prevents path traversal attacks.
pub fn check_containment(parent: &Path, child: &Path) -> Result<()> {
    // Use canonicalize if both paths exist, otherwise lexical check
    let (abs_parent, abs_child) = match (parent.canonicalize(), child.canonicalize()) {
        (Ok(p), Ok(c)) => (p, c),
        _ => {
            // Fallback to lexical normalization
            let p = normalize_lexical(parent);
            let c = normalize_lexical(child);
            (p, c)
        }
    };

    if abs_child.starts_with(&abs_parent) {
        Ok(())
    } else {
        Err(Error::Workspace(format!(
            "path {} is not contained within {}",
            child.display(),
            parent.display()
        )))
    }
}

/// Simple lexical path normalization (resolve `.` and `..` without touching the filesystem).
fn normalize_lexical(path: &Path) -> std::path::PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                components.pop();
            }
            std::path::Component::CurDir => {}
            other => components.push(other),
        }
    }
    components.iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_normal_key() {
        assert_eq!(sanitize_key("TASK-123456"), "TASK-123456");
    }

    #[test]
    fn sanitize_path_traversal() {
        assert_eq!(sanitize_key("foo/../bar"), "foo-bar");
    }

    #[test]
    fn sanitize_double_slash() {
        assert_eq!(sanitize_key("a//b"), "a-b");
    }

    #[test]
    fn sanitize_special_chars() {
        assert_eq!(sanitize_key("hello world!@#"), "hello-world");
    }

    #[test]
    fn sanitize_leading_trailing() {
        assert_eq!(sanitize_key("..foo.."), "foo");
    }

    #[test]
    fn containment_valid() {
        assert!(check_containment(Path::new("/a/b"), Path::new("/a/b/c")).is_ok());
    }

    #[test]
    fn containment_invalid() {
        assert!(check_containment(Path::new("/a/b"), Path::new("/a/c")).is_err());
    }

    #[test]
    fn containment_exact_match() {
        // Parent itself is considered contained
        assert!(check_containment(Path::new("/a/b"), Path::new("/a/b")).is_ok());
    }
}
