use std::fmt::Write;

use crate::model::{
    AnalysisDiagnostic, FindingKind, Outcome, SCHEMA_VERSION, Summary, ValidationReport,
};

pub fn render_text(report: &ValidationReport) -> String {
    let mut output = String::new();
    match report.outcome {
        Outcome::Passed => {
            writeln!(
                output,
                "architecture validation passed: {} modules, {} dependencies",
                report.summary.modules, report.summary.dependencies
            )
            .expect("writing to a String cannot fail");
        }
        Outcome::Violations => {
            writeln!(
                output,
                "architecture validation found {} violation(s):",
                report.summary.violations
            )
            .expect("writing to a String cannot fail");
            render_findings(&mut output, report);
        }
        Outcome::AnalysisFailure => {
            writeln!(
                output,
                "architecture analysis failed with {} error(s):",
                report.summary.analysis_errors
            )
            .expect("writing to a String cannot fail");
            for diagnostic in &report.analysis_errors {
                let location = match (&diagnostic.path, diagnostic.line) {
                    (Some(path), Some(line)) => format!(" at {path}:{line}"),
                    (Some(path), None) => format!(" at {path}"),
                    _ => String::new(),
                };
                writeln!(
                    output,
                    "error[{}] {}{}",
                    diagnostic.code, diagnostic.message, location
                )
                .expect("writing to a String cannot fail");
            }
            if !report.findings.is_empty() {
                writeln!(
                    output,
                    "architecture validation also found {} violation(s):",
                    report.summary.violations
                )
                .expect("writing to a String cannot fail");
                render_findings(&mut output, report);
            }
        }
    }
    for exemption in &report.exemptions {
        writeln!(
            output,
            "allowed[{}] exempted forbidden[{}] {} -> {} at {}:{} ({})",
            exemption.allowed_rule_id,
            exemption.forbidden_rule_id,
            exemption.source,
            exemption.target,
            exemption.evidence.path,
            exemption.evidence.line,
            exemption.evidence.expression
        )
        .expect("writing to a String cannot fail");
    }
    output
}

fn render_findings(output: &mut String, report: &ValidationReport) {
    for finding in &report.findings {
        match finding.kind {
            FindingKind::ForbiddenDependency => {
                let evidence = finding
                    .evidence
                    .as_ref()
                    .expect("dependency findings carry evidence");
                writeln!(
                    output,
                    "error[{}] {} -> {} at {}:{} ({})",
                    finding.rule_id,
                    finding.source,
                    finding.target.as_deref().unwrap_or("<unknown>"),
                    evidence.path,
                    evidence.line,
                    evidence.expression
                )
                .expect("writing to a String cannot fail");
            }
            FindingKind::Cycle => {
                writeln!(
                    output,
                    "error[{}] {}",
                    finding.rule_id,
                    finding.cycle.join(" -> ")
                )
                .expect("writing to a String cannot fail");
            }
        }
    }
}

pub fn render_json(report: &ValidationReport) -> anyhow::Result<String> {
    let mut output = serde_json::to_string_pretty(report)?;
    output.push('\n');
    Ok(output)
}

pub fn render_error_json(message: &str) -> anyhow::Result<String> {
    render_json(&analysis_failure_report(message))
}

pub fn analysis_failure_report(message: &str) -> ValidationReport {
    ValidationReport {
        schema_version: SCHEMA_VERSION,
        tool_version: env!("CARGO_PKG_VERSION"),
        outcome: Outcome::AnalysisFailure,
        summary: Summary {
            modules: 0,
            dependencies: 0,
            violations: 0,
            analysis_errors: 1,
            exemptions: 0,
        },
        modules: Vec::new(),
        dependencies: Vec::new(),
        findings: Vec::new(),
        exemptions: Vec::new(),
        analysis_errors: vec![AnalysisDiagnostic {
            code: "configuration-or-analysis-error".to_owned(),
            message: message.to_owned(),
            path: None,
            line: None,
        }],
        limitations: Vec::new(),
    }
}
