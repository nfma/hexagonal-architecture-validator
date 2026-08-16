use std::collections::{BTreeMap, BTreeSet};

use crate::config::{LoadedConfig, classify_modules};
use crate::model::{
    AnalysisDiagnostic, AppliedExemption, DependencyGraph, Finding, FindingKind, Outcome,
    SCHEMA_VERSION, Summary, ValidationReport,
};

const LIMITATIONS: [&str; 5] = [
    "cfg predicates are not evaluated; repeated same-source bodies are all analyzed, differing files fail, and conditional path attributes fail closed",
    "procedural and declarative macros are not expanded",
    "cross-role dependencies through public re-exports fail closed; the full pub-use graph is not followed",
    "method calls, dynamic dispatch, and runtime relationships do not create dependency edges",
    "a passing result is evidence for declared static boundaries, not proof of architectural quality",
];

pub fn evaluate(mut graph: DependencyGraph, config: &LoadedConfig) -> ValidationReport {
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
    record_unmatched_roles(&mut graph, config, &classifications);
    record_opaque_reexports(&mut graph, config, &classifications);
    let (mut findings, exemptions) = evaluate_rules(&graph, config, &classifications);
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
        exemptions: exemptions.len(),
    };

    ValidationReport {
        schema_version: SCHEMA_VERSION,
        tool_version: env!("CARGO_PKG_VERSION"),
        outcome,
        summary,
        modules,
        dependencies,
        findings,
        exemptions,
        analysis_errors,
        limitations: LIMITATIONS.to_vec(),
    }
}

fn evaluate_rules(
    graph: &DependencyGraph,
    config: &LoadedConfig,
    classifications: &BTreeMap<String, BTreeSet<String>>,
) -> (Vec<Finding>, Vec<AppliedExemption>) {
    let mut findings = Vec::new();
    let mut exemptions = BTreeSet::new();
    for dependency in &graph.dependencies {
        let source_roles = classifications
            .get(&dependency.source)
            .expect("all graph modules were classified");
        let target_roles = classifications
            .get(&dependency.target)
            .expect("all graph modules were classified");

        for rule in config
            .forbidden
            .iter()
            .filter(|rule| rule.matches(source_roles, target_roles))
        {
            let matching_exemptions = config.allowed.iter().filter(|allowed| {
                allowed.exempts.contains(&rule.id) && allowed.matches(source_roles, target_roles)
            });
            let mut exempted = false;
            for allowed in matching_exemptions {
                exempted = true;
                exemptions.insert(AppliedExemption {
                    allowed_rule_id: allowed.id.clone(),
                    forbidden_rule_id: rule.id.clone(),
                    source: dependency.source.clone(),
                    target: dependency.target.clone(),
                    evidence: dependency.evidence.clone(),
                });
            }
            if exempted {
                continue;
            }
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
    (findings, exemptions.into_iter().collect())
}

fn record_unmatched_roles(
    graph: &mut DependencyGraph,
    config: &LoadedConfig,
    classifications: &BTreeMap<String, BTreeSet<String>>,
) {
    for role in &config.roles {
        if classifications
            .values()
            .all(|roles| !roles.contains(&role.id))
        {
            graph.diagnostics.insert(AnalysisDiagnostic {
                code: "role-matched-no-modules".to_owned(),
                message: format!("declared role '{}' matched no discovered modules", role.id),
                path: None,
                line: None,
            });
        }
    }
}

fn record_opaque_reexports(
    graph: &mut DependencyGraph,
    config: &LoadedConfig,
    classifications: &BTreeMap<String, BTreeSet<String>>,
) {
    for reexport in &graph.opaque_reexports {
        let source_roles = classifications
            .get(&reexport.source)
            .expect("opaque re-export source was classified");
        let target_roles = classifications
            .get(&reexport.target)
            .expect("opaque re-export target was classified");
        let crosses_forbidden_via = classifications.get(&reexport.via).is_some_and(|via_roles| {
            config
                .forbidden
                .iter()
                .any(|rule| rule.matches(source_roles, via_roles))
        });
        if source_roles != target_roles
            || config
                .forbidden
                .iter()
                .any(|rule| rule.matches(source_roles, target_roles))
            || crosses_forbidden_via
        {
            graph.diagnostics.insert(AnalysisDiagnostic {
                code: "opaque-reexport".to_owned(),
                message: format!(
                    "dependency '{}' crosses a role boundary through re-export '{}' in '{}'; v0.1 does not follow the full pub-use graph",
                    reexport.evidence.expression, reexport.exported_name, reexport.via
                ),
                path: Some(reexport.evidence.path.clone()),
                line: Some(reexport.evidence.line),
            });
        }
    }
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
