use std::collections::{BTreeMap, BTreeSet};

use crate::config::{LoadedConfig, classify_modules};
use crate::model::{DependencyGraph, Finding, FindingKind, Outcome, Summary, ValidationReport};

const LIMITATIONS: [&str; 4] = [
    "cfg predicates are not evaluated; all syntactically present branches are analyzed",
    "procedural and declarative macros are not expanded",
    "method calls, dynamic dispatch, and runtime relationships do not create dependency edges",
    "a passing result is evidence for declared static boundaries, not proof of architectural quality",
];

pub fn evaluate(graph: DependencyGraph, config: &LoadedConfig) -> ValidationReport {
    let classifications = classify_modules(
        config,
        graph.modules.values().map(|module| {
            (
                module.id.clone(),
                module.module.clone(),
                module.source.clone(),
            )
        }),
    );
    let mut findings = evaluate_rules(&graph, config, &classifications);
    if config.analysis.detect_cycles {
        findings.extend(cycle_findings(&graph));
    }
    findings.sort();

    let modules = graph.modules.into_values().collect::<Vec<_>>();
    let dependencies = graph.dependencies.into_iter().collect::<Vec<_>>();
    let analysis_errors = graph.diagnostics.into_iter().collect::<Vec<_>>();
    let outcome = if !analysis_errors.is_empty() {
        Outcome::AnalysisFailure
    } else if !findings.is_empty() {
        Outcome::Violations
    } else {
        Outcome::Passed
    };
    let summary = Summary {
        modules: modules.len(),
        dependencies: dependencies.len(),
        violations: findings.len(),
        analysis_errors: analysis_errors.len(),
    };

    ValidationReport {
        schema_version: 1,
        tool_version: env!("CARGO_PKG_VERSION"),
        outcome,
        summary,
        modules,
        dependencies,
        findings,
        analysis_errors,
        limitations: LIMITATIONS.to_vec(),
    }
}

fn evaluate_rules(
    graph: &DependencyGraph,
    config: &LoadedConfig,
    classifications: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    for dependency in &graph.dependencies {
        let source_roles = classifications
            .get(&dependency.source)
            .expect("all graph modules were classified");
        let target_roles = classifications
            .get(&dependency.target)
            .expect("all graph modules were classified");

        if config
            .allowed
            .iter()
            .any(|rule| rule.matches(source_roles, target_roles))
        {
            continue;
        }
        for rule in config
            .forbidden
            .iter()
            .filter(|rule| rule.matches(source_roles, target_roles))
        {
            let rationale = rule
                .description
                .as_deref()
                .unwrap_or("dependency is forbidden by configuration");
            findings.push(Finding {
                rule_id: rule.id.clone(),
                kind: FindingKind::ForbiddenDependency,
                message: rationale.to_owned(),
                source: dependency.source.clone(),
                target: Some(dependency.target.clone()),
                evidence: Some(dependency.evidence.clone()),
                cycle: Vec::new(),
            });
        }
    }
    findings
}

fn cycle_findings(graph: &DependencyGraph) -> Vec<Finding> {
    strongly_connected_components(graph)
        .into_iter()
        .filter(|component| component.len() > 1)
        .map(|component| Finding {
            rule_id: "no-cycles".to_owned(),
            kind: FindingKind::Cycle,
            message: format!("dependency cycle contains {} modules", component.len()),
            source: component[0].clone(),
            target: None,
            evidence: None,
            cycle: component,
        })
        .collect()
}

fn strongly_connected_components(graph: &DependencyGraph) -> Vec<Vec<String>> {
    let mut adjacency = graph
        .modules
        .keys()
        .map(|node| (node.clone(), BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    for dependency in &graph.dependencies {
        adjacency
            .entry(dependency.source.clone())
            .or_default()
            .insert(dependency.target.clone());
    }

    let mut state = TarjanState::default();
    for node in adjacency.keys() {
        if !state.indices.contains_key(node) {
            connect(node, &adjacency, &mut state);
        }
    }
    for component in &mut state.components {
        component.sort();
    }
    state.components.sort();
    state.components
}

#[derive(Default)]
struct TarjanState {
    next_index: usize,
    indices: BTreeMap<String, usize>,
    low_links: BTreeMap<String, usize>,
    stack: Vec<String>,
    on_stack: BTreeSet<String>,
    components: Vec<Vec<String>>,
}

fn connect(node: &str, adjacency: &BTreeMap<String, BTreeSet<String>>, state: &mut TarjanState) {
    let index = state.next_index;
    state.next_index += 1;
    state.indices.insert(node.to_owned(), index);
    state.low_links.insert(node.to_owned(), index);
    state.stack.push(node.to_owned());
    state.on_stack.insert(node.to_owned());

    if let Some(neighbors) = adjacency.get(node) {
        for neighbor in neighbors {
            if !state.indices.contains_key(neighbor) {
                connect(neighbor, adjacency, state);
                let neighbor_low = state.low_links[neighbor];
                let node_low = state.low_links[node];
                state
                    .low_links
                    .insert(node.to_owned(), node_low.min(neighbor_low));
            } else if state.on_stack.contains(neighbor) {
                let neighbor_index = state.indices[neighbor];
                let node_low = state.low_links[node];
                state
                    .low_links
                    .insert(node.to_owned(), node_low.min(neighbor_index));
            }
        }
    }

    if state.low_links[node] == state.indices[node] {
        let mut component = Vec::new();
        while let Some(member) = state.stack.pop() {
            state.on_stack.remove(&member);
            component.push(member.clone());
            if member == node {
                break;
            }
        }
        state.components.push(component);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Dependency, Evidence, Module};

    #[test]
    fn finds_sorted_strongly_connected_components() {
        let mut graph = DependencyGraph::default();
        for id in ["a", "b", "c"] {
            graph.modules.insert(
                id.to_owned(),
                Module {
                    id: id.to_owned(),
                    package: "p".to_owned(),
                    target: "p::lib(p)".to_owned(),
                    module: id.to_owned(),
                    source: format!("src/{id}.rs"),
                },
            );
        }
        for (source, target) in [("a", "b"), ("b", "a"), ("b", "c")] {
            graph.dependencies.insert(Dependency {
                source: source.to_owned(),
                target: target.to_owned(),
                evidence: Evidence {
                    path: "src/lib.rs".to_owned(),
                    line: 1,
                    expression: target.to_owned(),
                },
            });
        }

        assert_eq!(
            strongly_connected_components(&graph),
            vec![vec!["a".to_owned(), "b".to_owned()], vec!["c".to_owned()]]
        );
    }
}
