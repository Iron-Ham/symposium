use crate::domain::issue::Issue;
use std::collections::{HashMap, HashSet};

/// State of a task in the epic dependency graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskState {
    /// Not started yet (e.g. "New", "To Do").
    NotStarted,
    /// Agent session is currently running.
    InProgress,
    /// PR has been created; branch name is known.
    HasPr(String),
    /// Terminal success state in Notion.
    Completed,
    /// Terminal cancelled/archived state.
    Cancelled,
}

/// Result of checking whether a task is eligible to be dispatched.
#[derive(Debug, Clone)]
pub struct EpicEligibility {
    /// Whether the task can be dispatched now.
    pub eligible: bool,
    /// The git base branch for this task's work.
    /// "main" for root tasks or fan-in tasks; a specific branch for stacked tasks.
    pub base_branch: String,
    /// Upstream blocker task identifiers that are still pending.
    pub unresolved: Vec<String>,
}

/// A dependency graph for an epic's sub-tasks.
///
/// Built from two sources:
/// 1. A Mermaid `graph TD` diagram on the epic page (authoritative)
/// 2. `Blocked by` native relations on individual tasks (supplementary)
///
/// Edges map: downstream_task → set of upstream_tasks that must resolve first.
#[derive(Debug, Clone, Default)]
pub struct EpicGraph {
    /// task_identifier → set of upstream task_identifiers that must complete first
    pub dependencies: HashMap<String, HashSet<String>>,
}

impl EpicGraph {
    /// Parse a Mermaid `graph TD` block and match node labels to task identifiers.
    ///
    /// Expected format:
    /// ```text
    /// graph TD
    ///   A["PM: Define requirements"]
    ///   B["Backend API endpoints"]
    ///   A --> B
    ///   A --> C
    /// ```
    ///
    /// Node labels are matched to task titles via normalized substring matching.
    pub fn from_mermaid(mermaid: &str, tasks: &[Issue]) -> Self {
        let mut graph = Self::default();

        // 1. Parse node definitions: ID["Label"] or ID["Label"]:::className
        let mut node_labels: HashMap<String, String> = HashMap::new();
        let node_re = regex::Regex::new(r#"(\w+)\["([^"]+)"\]"#).expect("valid regex");
        for cap in node_re.captures_iter(mermaid) {
            let node_id = cap[1].to_string();
            let label = cap[2].to_string();
            node_labels.insert(node_id, label);
        }

        // 2. Build label → task identifier mapping via normalized substring matching
        let label_to_task: HashMap<&str, &str> = node_labels
            .iter()
            .filter_map(|(node_id, label)| {
                match match_label_to_task(label, tasks) {
                    Some(task_id) => Some((label.as_str(), task_id)),
                    None => {
                        tracing::warn!(
                            node_id,
                            label,
                            "mermaid node label did not match any task title — \
                             this node and its edges will be missing from the dependency graph"
                        );
                        None
                    }
                }
            })
            .collect();

        // Also build node_id → task_identifier for edge resolution
        let node_to_task: HashMap<&str, &str> = node_labels
            .iter()
            .filter_map(|(node_id, label)| {
                let task_id = label_to_task.get(label.as_str())?;
                Some((node_id.as_str(), *task_id))
            })
            .collect();

        // 3. Parse edges: A --> B means A must complete before B starts.
        // Only match lines that look like standalone edges (not inside label strings).
        // We require the edge to be at the start of a line (after optional whitespace).
        let edge_re =
            regex::Regex::new(r"(?m)^\s*(\w+)\s*-->\s*(\w+)").expect("valid regex");
        for cap in edge_re.captures_iter(mermaid) {
            let from = &cap[1];
            let to = &cap[2];
            match (node_to_task.get(from), node_to_task.get(to)) {
                (Some(&upstream), Some(&downstream)) => {
                    graph
                        .dependencies
                        .entry(downstream.to_string())
                        .or_default()
                        .insert(upstream.to_string());
                }
                _ => {
                    tracing::warn!(
                        from,
                        to,
                        "mermaid edge references unresolved node(s) — edge skipped"
                    );
                }
            }
        }

        // Ensure all tasks have entries (even with no dependencies)
        for task in tasks {
            graph
                .dependencies
                .entry(task.identifier.clone())
                .or_default();
        }

        graph
    }

    /// Merge additional dependency edges from Notion "Blocked by" relations.
    pub fn merge_blocked_by(&mut self, task_id: &str, blocker_ids: &[String]) {
        let deps = self.dependencies.entry(task_id.to_string()).or_default();
        for blocker in blocker_ids {
            deps.insert(blocker.clone());
        }
    }

    /// Get upstream dependencies for a task.
    pub fn blockers(&self, task_id: &str) -> HashSet<String> {
        self.dependencies
            .get(task_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Check if a task is eligible to be dispatched given current task states.
    ///
    /// Branch stacking rules:
    /// - 0 unresolved blockers → eligible, base_branch = default_branch
    /// - 1 blocker resolved with PR → eligible, base_branch = blocker's branch (stacking)
    /// - N blockers all resolved → eligible, base_branch = default_branch (fan-in)
    /// - Any blocker not resolved → not eligible
    pub fn check_eligibility(
        &self,
        task_id: &str,
        task_states: &HashMap<String, TaskState>,
        default_branch: &str,
    ) -> EpicEligibility {
        let deps = self.blockers(task_id);

        if deps.is_empty() {
            return EpicEligibility {
                eligible: true,
                base_branch: default_branch.to_string(),
                unresolved: vec![],
            };
        }

        let mut unresolved = Vec::new();
        let mut resolved_branches: Vec<String> = Vec::new();

        for dep_id in &deps {
            match task_states.get(dep_id) {
                Some(TaskState::Completed | TaskState::Cancelled) => {
                    // Fully resolved, no branch to stack on.
                    // Cancelled tasks are treated as resolved — if a blocker is cancelled,
                    // the downstream task is unblocked (the assumption is that the cancelled
                    // work is no longer needed, not that the epic is abandoned).
                }
                Some(TaskState::HasPr(branch)) => {
                    resolved_branches.push(branch.clone());
                }
                _ => {
                    // NotStarted, InProgress, or unknown — not resolved
                    unresolved.push(dep_id.clone());
                }
            }
        }

        if !unresolved.is_empty() {
            return EpicEligibility {
                eligible: false,
                base_branch: default_branch.to_string(),
                unresolved,
            };
        }

        // All blockers are resolved
        let base_branch = if resolved_branches.len() == 1 {
            // Single blocker with PR → stack on it
            resolved_branches.into_iter().next().unwrap()
        } else {
            // 0 PR branches (all completed/cancelled) or fan-in (multiple PRs)
            default_branch.to_string()
        };

        EpicEligibility {
            eligible: true,
            base_branch,
            unresolved: vec![],
        }
    }
}

/// Extract a Mermaid code block from page content (fenced with ```mermaid).
pub fn extract_mermaid_block(content: &str) -> Option<&str> {
    let start_marker = "```mermaid";
    let start = content.find(start_marker)?;
    let block_start = start + start_marker.len();
    let end = content[block_start..].find("```")?;
    Some(content[block_start..block_start + end].trim())
}

/// Match a Mermaid node label to a task identifier via normalized substring matching.
///
/// Normalizes both the label and task titles to lowercase, strips common prefixes
/// like "[iOS]", "[Backend]", and compares.
fn match_label_to_task<'a>(label: &str, tasks: &'a [Issue]) -> Option<&'a str> {
    let normalized_label = normalize_for_matching(label);

    // Try exact normalized match first
    for task in tasks {
        let normalized_title = normalize_for_matching(&task.title);
        if normalized_label == normalized_title {
            return Some(&task.identifier);
        }
    }

    // Try substring match: label contained in title or title contained in label
    let mut best_match: Option<(&str, usize)> = None;
    for task in tasks {
        let normalized_title = normalize_for_matching(&task.title);
        let score = if normalized_title.contains(&normalized_label) {
            // Label is a substring of the title — shorter difference = better match
            normalized_title.len() - normalized_label.len()
        } else if normalized_label.contains(&normalized_title) {
            normalized_label.len() - normalized_title.len()
        } else {
            continue;
        };

        if best_match.is_none() || score < best_match.unwrap().1 {
            best_match = Some((&task.identifier, score));
        }
    }

    best_match.map(|(id, _)| id)
}

/// Normalize a string for fuzzy matching: lowercase, strip tag prefixes, collapse whitespace.
fn normalize_for_matching(s: &str) -> String {
    // Strip common tag prefixes like [iOS], [Backend], [All]
    let tag_re = regex::Regex::new(r"\[[\w\s]+\]\s*").expect("valid regex");
    let stripped = tag_re.replace_all(s, "");
    stripped.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_task(id: &str, title: &str) -> Issue {
        Issue {
            identifier: id.to_string(),
            title: title.to_string(),
            description: None,
            status: "New".to_string(),
            priority: None,
            url: None,
            notion_page_id: None,
            blockers: vec![],
            source: "notion".to_string(),
            extra: HashMap::new(),
            comments: vec![],
            workflow_id: String::new(),
        }
    }

    #[test]
    fn parse_simple_mermaid_graph() {
        let tasks = vec![
            make_task("TASK-1", "Define requirements"),
            make_task("TASK-2", "Backend API"),
            make_task("TASK-3", "iOS Gate"),
        ];

        let mermaid = r#"graph TD
    A["Define requirements"]
    B["Backend API"]
    C["iOS Gate"]
    A --> B
    A --> C"#;

        let graph = EpicGraph::from_mermaid(mermaid, &tasks);

        assert!(graph.blockers("TASK-1").is_empty());
        assert_eq!(
            graph.blockers("TASK-2"),
            HashSet::from(["TASK-1".to_string()])
        );
        assert_eq!(
            graph.blockers("TASK-3"),
            HashSet::from(["TASK-1".to_string()])
        );
    }

    #[test]
    fn parse_superarchive_style_mermaid() {
        let tasks = vec![
            make_task("TASK-1", "[PM] Define super-archive requirements"),
            make_task("TASK-2", "[Backend] API endpoints for archive"),
            make_task("TASK-3", "[iOS] Gate: swipe gesture framework"),
            make_task("TASK-4", "[iOS] Swipe action UI components"),
            make_task("TASK-5", "[Backend] GraphQL codegen"),
            make_task("TASK-6", "[iOS] Pending carousel"),
            make_task("TASK-7", "[All] End-to-end wiring"),
        ];

        let mermaid = r#"graph TD
    PM["Define super-archive requirements"]
    BE["API endpoints for archive"]
    GATE["Gate: swipe gesture framework"]
    SWIPE["Swipe action UI components"]
    GQL["GraphQL codegen"]
    PEND["Pending carousel"]
    E2E["End-to-end wiring"]
    PM --> BE
    PM --> GATE
    GATE --> SWIPE
    BE --> GQL
    GATE --> PEND
    GQL --> PEND
    SWIPE --> E2E
    PEND --> E2E
    GQL --> E2E
    BE --> E2E"#;

        let graph = EpicGraph::from_mermaid(mermaid, &tasks);

        // PM has no deps
        assert!(graph.blockers("TASK-1").is_empty());

        // Backend API depends on PM
        assert_eq!(
            graph.blockers("TASK-2"),
            HashSet::from(["TASK-1".to_string()])
        );

        // iOS Gate depends on PM
        assert_eq!(
            graph.blockers("TASK-3"),
            HashSet::from(["TASK-1".to_string()])
        );

        // Swipe depends on iOS Gate
        assert_eq!(
            graph.blockers("TASK-4"),
            HashSet::from(["TASK-3".to_string()])
        );

        // GraphQL depends on Backend API
        assert_eq!(
            graph.blockers("TASK-5"),
            HashSet::from(["TASK-2".to_string()])
        );

        // Pending carousel depends on iOS Gate + GraphQL
        assert_eq!(
            graph.blockers("TASK-6"),
            HashSet::from(["TASK-3".to_string(), "TASK-5".to_string()])
        );

        // E2E depends on Swipe + Pending + GraphQL + Backend
        assert_eq!(
            graph.blockers("TASK-7"),
            HashSet::from([
                "TASK-4".to_string(),
                "TASK-6".to_string(),
                "TASK-5".to_string(),
                "TASK-2".to_string(),
            ])
        );
    }

    #[test]
    fn merge_blocked_by_adds_edges() {
        let tasks = vec![
            make_task("TASK-1", "First"),
            make_task("TASK-2", "Second"),
            make_task("TASK-3", "Third"),
        ];

        let mermaid = r#"graph TD
    A["First"]
    B["Second"]
    C["Third"]
    A --> B"#;

        let mut graph = EpicGraph::from_mermaid(mermaid, &tasks);
        // Mermaid only has A→B, add A→C from Blocked by relation
        graph.merge_blocked_by("TASK-3", &["TASK-1".to_string()]);

        assert_eq!(
            graph.blockers("TASK-3"),
            HashSet::from(["TASK-1".to_string()])
        );
    }

    #[test]
    fn eligibility_no_blockers() {
        let graph = EpicGraph {
            dependencies: HashMap::from([("TASK-1".to_string(), HashSet::new())]),
        };

        let states = HashMap::new();
        let result = graph.check_eligibility("TASK-1", &states, "main");
        assert!(result.eligible);
        assert_eq!(result.base_branch, "main");
        assert!(result.unresolved.is_empty());
    }

    #[test]
    fn eligibility_single_blocker_completed() {
        let graph = EpicGraph {
            dependencies: HashMap::from([(
                "TASK-2".to_string(),
                HashSet::from(["TASK-1".to_string()]),
            )]),
        };

        let states = HashMap::from([("TASK-1".to_string(), TaskState::Completed)]);
        let result = graph.check_eligibility("TASK-2", &states, "main");
        assert!(result.eligible);
        assert_eq!(result.base_branch, "main");
    }

    #[test]
    fn eligibility_single_blocker_has_pr_stacks() {
        let graph = EpicGraph {
            dependencies: HashMap::from([(
                "TASK-2".to_string(),
                HashSet::from(["TASK-1".to_string()]),
            )]),
        };

        let states = HashMap::from([(
            "TASK-1".to_string(),
            TaskState::HasPr("symposium/task-TASK-1".to_string()),
        )]);
        let result = graph.check_eligibility("TASK-2", &states, "main");
        assert!(result.eligible);
        assert_eq!(result.base_branch, "symposium/task-TASK-1");
    }

    #[test]
    fn eligibility_fan_in_multiple_prs() {
        let graph = EpicGraph {
            dependencies: HashMap::from([(
                "TASK-3".to_string(),
                HashSet::from(["TASK-1".to_string(), "TASK-2".to_string()]),
            )]),
        };

        let states = HashMap::from([
            (
                "TASK-1".to_string(),
                TaskState::HasPr("symposium/task-TASK-1".to_string()),
            ),
            (
                "TASK-2".to_string(),
                TaskState::HasPr("symposium/task-TASK-2".to_string()),
            ),
        ]);
        let result = graph.check_eligibility("TASK-3", &states, "main");
        assert!(result.eligible);
        // Fan-in: base off main since we can't stack on multiple branches
        assert_eq!(result.base_branch, "main");
    }

    #[test]
    fn eligibility_blocked_by_unresolved() {
        let graph = EpicGraph {
            dependencies: HashMap::from([(
                "TASK-2".to_string(),
                HashSet::from(["TASK-1".to_string()]),
            )]),
        };

        let states = HashMap::from([("TASK-1".to_string(), TaskState::NotStarted)]);
        let result = graph.check_eligibility("TASK-2", &states, "main");
        assert!(!result.eligible);
        assert_eq!(result.unresolved, vec!["TASK-1".to_string()]);
    }

    #[test]
    fn eligibility_mixed_resolved_and_unresolved() {
        let graph = EpicGraph {
            dependencies: HashMap::from([(
                "TASK-3".to_string(),
                HashSet::from(["TASK-1".to_string(), "TASK-2".to_string()]),
            )]),
        };

        let states = HashMap::from([
            (
                "TASK-1".to_string(),
                TaskState::HasPr("symposium/task-TASK-1".to_string()),
            ),
            ("TASK-2".to_string(), TaskState::InProgress),
        ]);
        let result = graph.check_eligibility("TASK-3", &states, "main");
        assert!(!result.eligible);
        assert_eq!(result.unresolved, vec!["TASK-2".to_string()]);
    }

    #[test]
    fn eligibility_blocker_cancelled_unblocks_downstream() {
        let graph = EpicGraph {
            dependencies: HashMap::from([(
                "TASK-2".to_string(),
                HashSet::from(["TASK-1".to_string()]),
            )]),
        };

        let states = HashMap::from([("TASK-1".to_string(), TaskState::Cancelled)]);
        let result = graph.check_eligibility("TASK-2", &states, "main");
        assert!(result.eligible);
        assert_eq!(result.base_branch, "main");
    }

    #[test]
    fn eligibility_unknown_task_treated_as_no_deps() {
        let graph = EpicGraph::default();
        let states = HashMap::new();
        let result = graph.check_eligibility("TASK-UNKNOWN", &states, "main");
        assert!(result.eligible);
        assert_eq!(result.base_branch, "main");
    }

    #[test]
    fn extract_mermaid_from_content() {
        let content = r#"# Epic: Super-archive

Some description here.

```mermaid
graph TD
    A["Task 1"]
    B["Task 2"]
    A --> B
```

More text after.
"#;
        let block = extract_mermaid_block(content).unwrap();
        assert!(block.contains("graph TD"));
        assert!(block.contains("A --> B"));
    }

    #[test]
    fn extract_mermaid_missing() {
        assert!(extract_mermaid_block("no mermaid here").is_none());
    }

    #[test]
    fn normalize_strips_tags() {
        assert_eq!(
            normalize_for_matching("[iOS] Gate: swipe gesture framework"),
            "gate: swipe gesture framework"
        );
        assert_eq!(
            normalize_for_matching("[Backend] API endpoints"),
            "api endpoints"
        );
    }

    #[test]
    fn match_label_to_task_with_tags() {
        let tasks = vec![
            make_task("TASK-1", "[iOS] Gate: swipe gesture framework"),
            make_task("TASK-2", "[Backend] API endpoints"),
        ];

        assert_eq!(
            match_label_to_task("Gate: swipe gesture framework", &tasks),
            Some("TASK-1")
        );
        assert_eq!(
            match_label_to_task("API endpoints", &tasks),
            Some("TASK-2")
        );
    }
}
