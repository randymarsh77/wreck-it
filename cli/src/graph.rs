//! Dependency graph export for task files.
//!
//! This module reads a wreck-it task list and emits a textual representation
//! of the dependency graph in either Mermaid flowchart or GraphViz DOT format.
//!
//! ## Mermaid
//!
//! ```mermaid
//! flowchart TD
//!   t1["t1\nimplementer"] --> t2
//!   style t1 fill:#90ee90,stroke:#333,color:#000
//! ```
//!
//! ## DOT
//!
//! ```dot
//! digraph tasks {
//!   "t1" [label="t1\nimplementer", style=filled, fillcolor="#90ee90"];
//! }
//! ```

use crate::types::{AgentRole, Task, TaskStatus};
use clap::ValueEnum;
use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------------------
// GraphFormat
// ---------------------------------------------------------------------------

/// Output format for the dependency graph export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum GraphFormat {
    /// Mermaid flowchart syntax (default).
    #[default]
    Mermaid,
    /// GraphViz DOT language.
    Dot,
}

// ---------------------------------------------------------------------------
// Status colours
// ---------------------------------------------------------------------------

fn status_fill_hex(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "#d3d3d3",    // light gray
        TaskStatus::InProgress => "#add8e6", // light blue
        TaskStatus::Completed => "#90ee90",  // light green
        TaskStatus::Failed => "#ff7f7f",     // light red
    }
}

// ---------------------------------------------------------------------------
// Mermaid
// ---------------------------------------------------------------------------

fn role_label(role: AgentRole) -> &'static str {
    match role {
        AgentRole::Ideas => "ideas",
        AgentRole::Implementer => "implementer",
        AgentRole::Evaluator => "evaluator",
        AgentRole::SecurityGate => "security_gate",
        AgentRole::CoverageEnforcer => "coverage_enforcer",
        AgentRole::ChangelogGenerator => "changelog_generator",
    }
}

/// Generate a Mermaid flowchart representation of the task dependency graph.
///
/// Each node is labelled with its `id` and `role`.  Nodes are coloured by
/// status: pending=gray, in_progress=blue, completed=green, failed=red.
/// Directed edges represent `depends_on` relationships.
pub fn generate_mermaid(tasks: &[Task]) -> String {
    let mut out = String::from("flowchart TD\n");

    // Emit nodes with labels and style.
    for task in tasks {
        let fill = status_fill_hex(task.status);
        let label = format!("{}\\n{}", task.id, role_label(task.role));
        // Node declaration with label
        out.push_str(&format!("  {}[\"{label}\"]\n", mermaid_id(&task.id)));
        // Inline style
        out.push_str(&format!(
            "  style {} fill:{fill},stroke:#333,color:#000\n",
            mermaid_id(&task.id)
        ));
    }

    // Emit edges.
    for task in tasks {
        for dep in &task.depends_on {
            out.push_str(&format!(
                "  {} --> {}\n",
                mermaid_id(dep),
                mermaid_id(&task.id)
            ));
        }
    }

    out
}

/// Sanitise a task id for use as a Mermaid node identifier.
///
/// Mermaid node IDs must not contain hyphens or special characters that would
/// confuse the parser.  We replace `-` with `_` and strip other non-word chars.
fn mermaid_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Cycle detection
// ---------------------------------------------------------------------------

/// Detect cycles in the task dependency graph using iterative DFS.
///
/// Returns a list of cycles, each represented as a `Vec<String>` of task IDs
/// forming the cycle (the first and last element are the same to close the
/// loop).  An empty return value means the graph is acyclic.
pub fn detect_cycles(tasks: &[Task]) -> Vec<Vec<String>> {
    // Build adjacency list: id → list of ids it depends on
    let adj: HashMap<&str, Vec<&str>> = tasks
        .iter()
        .map(|t| {
            (
                t.id.as_str(),
                t.depends_on.iter().map(String::as_str).collect(),
            )
        })
        .collect();

    let mut visited: HashSet<&str> = HashSet::new();
    let mut cycles: Vec<Vec<String>> = Vec::new();

    for start in adj.keys().copied() {
        if visited.contains(start) {
            continue;
        }
        // DFS with an explicit stack; each entry is (node, iterator over neighbours, current path).
        let mut path: Vec<&str> = Vec::new();
        let mut in_path: HashSet<&str> = HashSet::new();
        let mut stack: Vec<(&str, usize)> = vec![(start, 0)];

        while let Some((node, idx)) = stack.last_mut() {
            let node = *node;
            if *idx == 0 {
                // First visit of this node on this DFS branch.
                if visited.contains(node) {
                    stack.pop();
                    continue;
                }
                path.push(node);
                in_path.insert(node);
            }
            let neighbours = adj.get(node).map(Vec::as_slice).unwrap_or(&[]);
            if *idx < neighbours.len() {
                let next = neighbours[*idx];
                *idx += 1;
                if in_path.contains(next) {
                    // Found a cycle: collect nodes from `next` to end of path.
                    let cycle_start = path.iter().position(|&n| n == next).unwrap();
                    let mut cycle: Vec<String> =
                        path[cycle_start..].iter().map(|s| s.to_string()).collect();
                    cycle.push(next.to_string()); // close the loop
                    cycles.push(cycle);
                } else if !visited.contains(next) {
                    stack.push((next, 0));
                }
            } else {
                // All neighbours explored.
                visited.insert(node);
                in_path.remove(node);
                path.pop();
                stack.pop();
            }
        }
    }

    cycles
}

// ---------------------------------------------------------------------------
// DOT
// ---------------------------------------------------------------------------

/// Generate a GraphViz DOT representation of the task dependency graph.
///
/// Each node is labelled with its `id` and `role`.  Nodes are filled with a
/// colour derived from their status.  Directed edges represent `depends_on`
/// relationships.
pub fn generate_dot(tasks: &[Task]) -> String {
    let mut out = String::from("digraph tasks {\n");
    out.push_str("  rankdir=TD;\n");
    out.push_str("  node [shape=box];\n");

    // Emit node declarations.
    for task in tasks {
        let fill = status_fill_hex(task.status);
        let label = format!("{}\\n{}", task.id, role_label(task.role));
        out.push_str(&format!(
            "  \"{}\" [label=\"{label}\", style=filled, fillcolor=\"{fill}\"];\n",
            dot_escape(&task.id)
        ));
    }

    // Emit edges.
    for task in tasks {
        for dep in &task.depends_on {
            out.push_str(&format!(
                "  \"{}\" -> \"{}\";\n",
                dot_escape(dep),
                dot_escape(&task.id)
            ));
        }
    }

    out.push_str("}\n");
    out
}

/// Escape a string for use inside a DOT double-quoted label/id.
fn dot_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AgentRole, TaskKind, TaskRuntime, TaskStatus};

    fn make_task(id: &str, status: TaskStatus, role: AgentRole, depends_on: Vec<&str>) -> Task {
        Task {
            id: id.to_string(),
            description: format!("task {id}"),
            status,
            role,
            kind: TaskKind::default(),
            cooldown_seconds: None,
            phase: 1,
            depends_on: depends_on.into_iter().map(String::from).collect(),
            priority: 0,
            complexity: 1,
            timeout_seconds: None,
            max_retries: None,
            failed_attempts: 0,
            last_attempt_at: None,
            inputs: vec![],
            outputs: vec![],
            runtime: TaskRuntime::default(),
            precondition_prompt: None,
            parent_id: None,
            labels: vec![],
            system_prompt_override: None,
            acceptance_criteria: None,
            evaluation: None,
        }
    }

    // ---- GraphFormat ----

    #[test]
    fn graph_format_default_is_mermaid() {
        assert_eq!(GraphFormat::default(), GraphFormat::Mermaid);
    }

    // ---- status_fill_hex ----

    #[test]
    fn pending_is_gray() {
        assert_eq!(status_fill_hex(TaskStatus::Pending), "#d3d3d3");
    }

    #[test]
    fn completed_is_green() {
        assert_eq!(status_fill_hex(TaskStatus::Completed), "#90ee90");
    }

    #[test]
    fn failed_is_red() {
        assert_eq!(status_fill_hex(TaskStatus::Failed), "#ff7f7f");
    }

    #[test]
    fn in_progress_is_blue() {
        assert_eq!(status_fill_hex(TaskStatus::InProgress), "#add8e6");
    }

    // ---- mermaid_id ----

    #[test]
    fn mermaid_id_replaces_hyphens() {
        assert_eq!(mermaid_id("impl-task-1"), "impl_task_1");
    }

    #[test]
    fn mermaid_id_keeps_underscores() {
        assert_eq!(mermaid_id("my_task"), "my_task");
    }

    // ---- generate_mermaid ----

    #[test]
    fn mermaid_empty_task_list_produces_header() {
        let out = generate_mermaid(&[]);
        assert!(out.starts_with("flowchart TD\n"));
        assert_eq!(out.trim(), "flowchart TD");
    }

    #[test]
    fn mermaid_single_node_appears_with_label_and_style() {
        let tasks = vec![make_task(
            "t1",
            TaskStatus::Completed,
            AgentRole::Implementer,
            vec![],
        )];
        let out = generate_mermaid(&tasks);
        assert!(
            out.contains("t1[\"t1\\nimplementer\"]"),
            "node label missing"
        );
        assert!(out.contains("fill:#90ee90"), "status colour missing");
    }

    #[test]
    fn mermaid_edge_represents_depends_on() {
        let tasks = vec![
            make_task("a", TaskStatus::Completed, AgentRole::Implementer, vec![]),
            make_task("b", TaskStatus::Pending, AgentRole::Implementer, vec!["a"]),
        ];
        let out = generate_mermaid(&tasks);
        assert!(
            out.contains("a --> b"),
            "edge 'a --> b' missing from:\n{out}"
        );
    }

    #[test]
    fn mermaid_hyphenated_ids_are_sanitised_in_edges() {
        let tasks = vec![
            make_task("ideas-1", TaskStatus::Completed, AgentRole::Ideas, vec![]),
            make_task(
                "impl-1",
                TaskStatus::Pending,
                AgentRole::Implementer,
                vec!["ideas-1"],
            ),
        ];
        let out = generate_mermaid(&tasks);
        // edges should use sanitised IDs
        assert!(
            out.contains("ideas_1 --> impl_1"),
            "sanitised edge missing:\n{out}"
        );
    }

    #[test]
    fn mermaid_failed_task_is_red() {
        let tasks = vec![make_task(
            "t1",
            TaskStatus::Failed,
            AgentRole::Implementer,
            vec![],
        )];
        let out = generate_mermaid(&tasks);
        assert!(out.contains("fill:#ff7f7f"), "failed colour missing");
    }

    #[test]
    fn mermaid_ideas_role_label() {
        let tasks = vec![make_task(
            "t1",
            TaskStatus::Pending,
            AgentRole::Ideas,
            vec![],
        )];
        let out = generate_mermaid(&tasks);
        assert!(out.contains("ideas"), "ideas role label missing");
    }

    // ---- generate_dot ----

    #[test]
    fn dot_empty_task_list_produces_digraph() {
        let out = generate_dot(&[]);
        assert!(out.starts_with("digraph tasks {"), "missing digraph header");
        assert!(out.trim_end().ends_with('}'), "missing closing brace");
    }

    #[test]
    fn dot_single_node_has_label_and_fillcolor() {
        let tasks = vec![make_task(
            "t1",
            TaskStatus::Pending,
            AgentRole::Implementer,
            vec![],
        )];
        let out = generate_dot(&tasks);
        assert!(out.contains("\"t1\""), "node id missing");
        assert!(
            out.contains("fillcolor=\"#d3d3d3\""),
            "pending fillcolor missing"
        );
        assert!(out.contains("implementer"), "role label missing");
    }

    #[test]
    fn dot_edge_represents_depends_on() {
        let tasks = vec![
            make_task("a", TaskStatus::Completed, AgentRole::Implementer, vec![]),
            make_task("b", TaskStatus::Pending, AgentRole::Implementer, vec!["a"]),
        ];
        let out = generate_dot(&tasks);
        assert!(out.contains("\"a\" -> \"b\""), "edge missing from:\n{out}");
    }

    #[test]
    fn dot_completed_task_is_green() {
        let tasks = vec![make_task(
            "t1",
            TaskStatus::Completed,
            AgentRole::Evaluator,
            vec![],
        )];
        let out = generate_dot(&tasks);
        assert!(
            out.contains("fillcolor=\"#90ee90\""),
            "completed fillcolor missing"
        );
    }

    #[test]
    fn dot_escape_handles_quotes_and_backslashes() {
        assert_eq!(dot_escape("a\"b"), "a\\\"b");
        assert_eq!(dot_escape("a\\b"), "a\\\\b");
    }

    // ---- 3-task chain (A → B → C) ----

    #[test]
    fn mermaid_three_task_chain_has_flowchart_header_and_arrows() {
        let tasks = vec![
            make_task("a", TaskStatus::Pending, AgentRole::Implementer, vec![]),
            make_task("b", TaskStatus::Pending, AgentRole::Implementer, vec!["a"]),
            make_task("c", TaskStatus::Pending, AgentRole::Implementer, vec!["b"]),
        ];
        let out = generate_mermaid(&tasks);
        // Correct flowchart header
        assert!(
            out.starts_with("flowchart TD\n"),
            "flowchart header missing"
        );
        // Arrow from A to B
        assert!(
            out.contains("a --> b"),
            "edge 'a --> b' missing from:\n{out}"
        );
        // Arrow from B to C
        assert!(
            out.contains("b --> c"),
            "edge 'b --> c' missing from:\n{out}"
        );
        // All three nodes declared
        assert!(out.contains("a["), "node 'a' missing");
        assert!(out.contains("b["), "node 'b' missing");
        assert!(out.contains("c["), "node 'c' missing");
    }

    #[test]
    fn dot_three_task_chain_has_digraph_header_nodes_and_edges() {
        let tasks = vec![
            make_task("a", TaskStatus::Pending, AgentRole::Implementer, vec![]),
            make_task("b", TaskStatus::Pending, AgentRole::Implementer, vec!["a"]),
            make_task("c", TaskStatus::Pending, AgentRole::Implementer, vec!["b"]),
        ];
        let out = generate_dot(&tasks);
        // Digraph header
        assert!(out.starts_with("digraph tasks {"), "digraph header missing");
        // Closing brace
        assert!(out.trim_end().ends_with('}'), "closing brace missing");
        // Node definitions
        assert!(out.contains("\"a\""), "node 'a' missing");
        assert!(out.contains("\"b\""), "node 'b' missing");
        assert!(out.contains("\"c\""), "node 'c' missing");
        // Edge declarations
        assert!(
            out.contains("\"a\" -> \"b\""),
            "edge a->b missing from:\n{out}"
        );
        assert!(
            out.contains("\"b\" -> \"c\""),
            "edge b->c missing from:\n{out}"
        );
    }

    // ---- isolated nodes (no dependencies) ----

    #[test]
    fn mermaid_isolated_nodes_have_no_edges() {
        let tasks = vec![
            make_task("x", TaskStatus::Pending, AgentRole::Implementer, vec![]),
            make_task("y", TaskStatus::Pending, AgentRole::Implementer, vec![]),
        ];
        let out = generate_mermaid(&tasks);
        // Both nodes declared
        assert!(out.contains("x["), "node 'x' missing");
        assert!(out.contains("y["), "node 'y' missing");
        // No arrow/edge between them
        assert!(
            !out.contains("-->"),
            "unexpected edges in isolated-node graph:\n{out}"
        );
    }

    #[test]
    fn dot_isolated_nodes_have_no_edges() {
        let tasks = vec![
            make_task("x", TaskStatus::Pending, AgentRole::Implementer, vec![]),
            make_task("y", TaskStatus::Pending, AgentRole::Implementer, vec![]),
        ];
        let out = generate_dot(&tasks);
        // Both node declarations present
        assert!(out.contains("\"x\""), "node 'x' missing");
        assert!(out.contains("\"y\""), "node 'y' missing");
        // No directed edges (edge syntax is `"x" -> "y"`)
        assert!(
            !out.contains("\" -> \""),
            "unexpected edges in isolated-node graph:\n{out}"
        );
    }

    // ---- detect_cycles ----

    #[test]
    fn detect_cycles_empty_graph_returns_no_cycles() {
        let cycles = detect_cycles(&[]);
        assert!(cycles.is_empty(), "empty graph should have no cycles");
    }

    #[test]
    fn detect_cycles_acyclic_chain_returns_no_cycles() {
        let tasks = vec![
            make_task("a", TaskStatus::Pending, AgentRole::Implementer, vec![]),
            make_task("b", TaskStatus::Pending, AgentRole::Implementer, vec!["a"]),
            make_task("c", TaskStatus::Pending, AgentRole::Implementer, vec!["b"]),
        ];
        let cycles = detect_cycles(&tasks);
        assert!(
            cycles.is_empty(),
            "acyclic chain should have no cycles: {cycles:?}"
        );
    }

    #[test]
    fn detect_cycles_self_loop_is_detected() {
        // a depends_on a (self-loop)
        let tasks = vec![make_task(
            "a",
            TaskStatus::Pending,
            AgentRole::Implementer,
            vec!["a"],
        )];
        let cycles = detect_cycles(&tasks);
        assert!(
            !cycles.is_empty(),
            "self-loop should be detected as a cycle"
        );
    }

    #[test]
    fn detect_cycles_two_node_cycle_is_detected() {
        // a → b → a
        let tasks = vec![
            make_task("a", TaskStatus::Pending, AgentRole::Implementer, vec!["b"]),
            make_task("b", TaskStatus::Pending, AgentRole::Implementer, vec!["a"]),
        ];
        let cycles = detect_cycles(&tasks);
        assert!(!cycles.is_empty(), "two-node cycle should be detected");
        // Both 'a' and 'b' should appear in the cycle
        let all_nodes: Vec<&str> = cycles
            .iter()
            .flat_map(|c| c.iter().map(String::as_str))
            .collect();
        assert!(
            all_nodes.contains(&"a"),
            "'a' missing from cycle: {cycles:?}"
        );
        assert!(
            all_nodes.contains(&"b"),
            "'b' missing from cycle: {cycles:?}"
        );
    }

    #[test]
    fn detect_cycles_three_node_cycle_is_detected() {
        // a → b → c → a
        let tasks = vec![
            make_task("a", TaskStatus::Pending, AgentRole::Implementer, vec!["c"]),
            make_task("b", TaskStatus::Pending, AgentRole::Implementer, vec!["a"]),
            make_task("c", TaskStatus::Pending, AgentRole::Implementer, vec!["b"]),
        ];
        let cycles = detect_cycles(&tasks);
        assert!(!cycles.is_empty(), "three-node cycle should be detected");
    }

    #[test]
    fn detect_cycles_isolated_nodes_have_no_cycles() {
        let tasks = vec![
            make_task("x", TaskStatus::Pending, AgentRole::Implementer, vec![]),
            make_task("y", TaskStatus::Pending, AgentRole::Implementer, vec![]),
        ];
        let cycles = detect_cycles(&tasks);
        assert!(
            cycles.is_empty(),
            "isolated nodes should have no cycles: {cycles:?}"
        );
    }
}
