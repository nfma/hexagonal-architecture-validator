use std::fmt::Write;

use crate::model::{FindingKind, Outcome, ValidationReport};

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
        }
    }
    if matches!(report.outcome, Outcome::Passed | Outcome::Violations) {
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
    }
    output
}

pub fn render_json(report: &ValidationReport) -> anyhow::Result<String> {
    let mut output = serde_json::to_string_pretty(report)?;
    output.push('\n');
    Ok(output)
}

pub fn render_error_json(message: &str) -> anyhow::Result<String> {
    let mut output = serde_json::to_string_pretty(&serde_json::json!({
        "schema_version": 1,
        "tool_version": env!("CARGO_PKG_VERSION"),
        "outcome": "configuration-or-analysis-failure",
        "error": {
            "message": message,
        },
    }))?;
    output.push('\n');
    Ok(output)
}
