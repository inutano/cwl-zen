use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::{bail, Result};

use crate::model::{StepInput, Workflow};

/// A step in the execution DAG with its dependencies resolved.
#[derive(Debug, Clone)]
pub struct DagStep {
    pub name: String,
    pub tool_path: String,
    pub depends_on: Vec<String>,
}

/// Build a DAG from a workflow definition.
/// Returns steps in topological order (dependencies first).
pub fn build_dag(workflow: &Workflow) -> Result<Vec<DagStep>> {
    // 1. Collect all step names
    let step_names: HashSet<&str> = workflow.steps.keys().map(|s| s.as_str()).collect();

    // 2. For each step, resolve dependencies from its inputs
    let mut dag_steps: HashMap<&str, DagStep> = HashMap::new();
    for (name, step) in &workflow.steps {
        let mut depends_on = Vec::new();
        for input_val in step.inputs.values() {
            let source = match input_val {
                StepInput::Source(s) => Some(s.as_str()),
                StepInput::Structured(entry) => entry.source.as_deref(),
            };
            if let Some(src) = source {
                if let Some(dep_name) = src.split('/').next() {
                    if step_names.contains(dep_name) && !depends_on.contains(&dep_name.to_string())
                    {
                        depends_on.push(dep_name.to_string());
                    }
                }
            }
        }
        depends_on.sort(); // deterministic ordering
        dag_steps.insert(
            name.as_str(),
            DagStep {
                name: name.clone(),
                tool_path: step.run.clone(),
                depends_on,
            },
        );
    }

    // 3. Topological sort using Kahn's algorithm
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    for name in &step_names {
        in_degree.insert(name, 0);
    }
    for step in dag_steps.values() {
        for dep in &step.depends_on {
            if let Some(count) = in_degree.get_mut(dep.as_str()) {
                // dep has a dependent (current step), but in_degree tracks
                // how many deps point INTO a node, so we increment the
                // current step's in-degree instead.
                let _ = count; // unused
            }
        }
    }
    // Recompute correctly: in_degree[X] = number of steps that X depends on... no.
    // Kahn's: in_degree[X] = number of edges pointing into X = number of steps
    // that must complete before X can run. Wait, that's the dependency count.
    // Actually in_degree in Kahn's = number of incoming edges in the DAG.
    // Edge: dependency -> dependent. So in_degree[step] = step.depends_on.len()

    // Reset and compute properly
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    for name in &step_names {
        in_degree.insert(name, 0);
    }
    for step in dag_steps.values() {
        in_degree.insert(&step.name, step.depends_on.len());
    }

    // Build adjacency: for each dependency edge dep -> step, track dependents
    let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();
    for name in &step_names {
        dependents.insert(name, Vec::new());
    }
    for step in dag_steps.values() {
        for dep in &step.depends_on {
            if let Some(list) = dependents.get_mut(dep.as_str()) {
                list.push(&step.name);
            }
        }
    }

    // Kahn's
    let mut queue: VecDeque<&str> = VecDeque::new();
    let mut candidates: Vec<&str> = in_degree
        .iter()
        .filter(|(_, &deg)| deg == 0)
        .map(|(&name, _)| name)
        .collect();
    candidates.sort(); // deterministic
    for c in candidates {
        queue.push_back(c);
    }

    let mut sorted_names: Vec<String> = Vec::new();
    while let Some(name) = queue.pop_front() {
        sorted_names.push(name.to_string());
        // Gather dependents to process, sort for determinism
        let mut next: Vec<&str> = Vec::new();
        if let Some(deps) = dependents.get(name) {
            for &dep_name in deps {
                if let Some(deg) = in_degree.get_mut(dep_name) {
                    *deg -= 1;
                    if *deg == 0 {
                        next.push(dep_name);
                    }
                }
            }
        }
        next.sort();
        for n in next {
            queue.push_back(n);
        }
    }

    let mut sorted: Vec<DagStep> = Vec::new();
    for name in &sorted_names {
        if let Some(step) = dag_steps.remove(name.as_str()) {
            sorted.push(step);
        }
    }

    if sorted.len() != step_names.len() {
        bail!(
            "cyclic dependency detected among workflow steps (sorted {} of {})",
            sorted.len(),
            step_names.len()
        );
    }

    Ok(sorted)
}

/// Print the DAG as a human-readable dependency graph.
pub fn print_dag(steps: &[DagStep]) {
    for (i, step) in steps.iter().enumerate() {
        let deps = if step.depends_on.is_empty() {
            "(no dependencies)".to_string()
        } else {
            format!("depends on: {}", step.depends_on.join(", "))
        };
        println!(
            "  {}. {} [{}] — {}",
            i + 1,
            step.name,
            step.tool_path,
            deps
        );
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use crate::parse::parse_cwl;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    /// Helper to build a minimal Workflow with the given steps for testing.
    fn make_workflow(steps: HashMap<String, WorkflowStep>) -> Workflow {
        Workflow {
            cwl_version: Some("v1.2".to_string()),
            label: None,
            doc: None,
            inputs: HashMap::new(),
            outputs: HashMap::new(),
            steps,
            requirements: Vec::new(),
        }
    }

    /// Helper to build a WorkflowStep with source-based inputs.
    fn make_step(run: &str, sources: &[(&str, &str)]) -> WorkflowStep {
        let mut inputs = HashMap::new();
        for &(input_name, source) in sources {
            inputs.insert(
                input_name.to_string(),
                StepInput::Source(source.to_string()),
            );
        }
        WorkflowStep {
            run: run.to_string(),
            inputs,
            out: vec!["output".to_string()],
            scatter: None,
            scatter_method: None,
        }
    }

    // -- Test 1: two-step workflow from fixture --------------------------------

    #[test]
    fn dag_two_step_workflow() {
        let doc = parse_cwl(&fixture_path("two-step.cwl")).unwrap();
        let wf = match doc {
            CwlDocument::Workflow(wf) => wf,
            _ => panic!("expected Workflow"),
        };
        let dag = build_dag(&wf).unwrap();
        assert_eq!(dag.len(), 2);

        // echo_step should come first (no dependencies)
        assert_eq!(dag[0].name, "echo_step");
        assert!(dag[0].depends_on.is_empty());
        assert_eq!(dag[0].tool_path, "echo.cwl");

        // cat_step should come second (depends on echo_step)
        assert_eq!(dag[1].name, "cat_step");
        assert_eq!(dag[1].depends_on, vec!["echo_step"]);
        assert_eq!(dag[1].tool_path, "cat.cwl");
    }

    // -- Test 2: cyclic dependency detection -----------------------------------

    #[test]
    fn dag_detects_cycle() {
        let mut steps = HashMap::new();
        // A depends on B, B depends on A
        steps.insert("A".to_string(), make_step("a.cwl", &[("in1", "B/out")]));
        steps.insert("B".to_string(), make_step("b.cwl", &[("in1", "A/out")]));
        let wf = make_workflow(steps);
        let result = build_dag(&wf);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("cyclic"),
            "expected 'cyclic' in error: {err_msg}"
        );
    }

    // -- Test 3: parallel (independent) steps ----------------------------------

    #[test]
    fn dag_parallel_steps() {
        let mut steps = HashMap::new();
        // Two steps with no inter-dependencies (only workflow-level inputs)
        steps.insert(
            "step_a".to_string(),
            make_step("a.cwl", &[("in1", "workflow_input")]),
        );
        steps.insert(
            "step_b".to_string(),
            make_step("b.cwl", &[("in1", "another_input")]),
        );
        let wf = make_workflow(steps);
        let dag = build_dag(&wf).unwrap();
        assert_eq!(dag.len(), 2);

        // Both should have no dependencies
        for step in &dag {
            assert!(
                step.depends_on.is_empty(),
                "step {} should have no deps, got {:?}",
                step.name,
                step.depends_on
            );
        }
    }

    // -- Test 4: diamond pattern A -> B, A -> C, B -> D, C -> D ---------------

    #[test]
    fn dag_diamond() {
        let mut steps = HashMap::new();
        steps.insert(
            "A".to_string(),
            make_step("a.cwl", &[("in1", "workflow_input")]),
        );
        steps.insert(
            "B".to_string(),
            make_step("b.cwl", &[("in1", "A/out")]),
        );
        steps.insert(
            "C".to_string(),
            make_step("c.cwl", &[("in1", "A/out")]),
        );
        steps.insert(
            "D".to_string(),
            make_step("d.cwl", &[("in1", "B/out"), ("in2", "C/out")]),
        );
        let wf = make_workflow(steps);
        let dag = build_dag(&wf).unwrap();
        assert_eq!(dag.len(), 4);

        // A must be first (no deps)
        assert_eq!(dag[0].name, "A");
        assert!(dag[0].depends_on.is_empty());

        // D must be last (depends on B and C)
        assert_eq!(dag[3].name, "D");
        assert!(dag[3].depends_on.contains(&"B".to_string()));
        assert!(dag[3].depends_on.contains(&"C".to_string()));

        // B and C are in the middle (positions 1 and 2), order is deterministic
        let middle: Vec<&str> = vec![&dag[1].name, &dag[2].name]
            .into_iter()
            .map(|s| s.as_str())
            .collect();
        assert!(middle.contains(&"B"));
        assert!(middle.contains(&"C"));

        // B and C each depend only on A
        for step in &dag[1..3] {
            assert_eq!(step.depends_on, vec!["A"]);
        }
    }
}
