use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Serialize)]
pub struct Module {
    pub id: String,
    pub package: String,
    pub target: String,
    pub module: String,
    pub source: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Serialize)]
pub struct Dependency {
    pub source: String,
    pub target: String,
    pub evidence: Evidence,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Serialize)]
pub struct Evidence {
    pub path: String,
    pub line: usize,
    pub expression: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct OpaqueReexport {
    pub source: String,
    pub target: String,
    pub via: String,
    pub exported_name: String,
    pub evidence: Evidence,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Serialize)]
pub struct AnalysisDiagnostic {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
}

#[derive(Debug, Default)]
pub struct DependencyGraph {
    pub modules: BTreeMap<String, Module>,
    pub dependencies: BTreeSet<Dependency>,
    pub opaque_reexports: BTreeSet<OpaqueReexport>,
    pub diagnostics: BTreeSet<AnalysisDiagnostic>,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Serialize)]
pub struct AppliedExemption {
    pub allowed_rule_id: String,
    pub forbidden_rule_id: String,
    pub source: String,
    pub target: String,
    pub evidence: Evidence,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FindingKind {
    ForbiddenDependency,
    Cycle,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Serialize)]
pub struct Finding {
    pub rule_id: String,
    pub kind: FindingKind,
    pub message: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cycle: Vec<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    Passed,
    Violations,
    AnalysisFailure,
}

#[derive(Debug, Serialize)]
pub struct Summary {
    pub modules: usize,
    pub dependencies: usize,
    pub violations: usize,
    pub analysis_errors: usize,
    pub exemptions: usize,
}

#[derive(Debug, Serialize)]
pub struct ValidationReport {
    pub schema_version: u32,
    pub tool_version: &'static str,
    pub outcome: Outcome,
    pub summary: Summary,
    pub modules: Vec<Module>,
    pub dependencies: Vec<Dependency>,
    pub findings: Vec<Finding>,
    pub exemptions: Vec<AppliedExemption>,
    pub analysis_errors: Vec<AnalysisDiagnostic>,
    pub limitations: Vec<&'static str>,
}
