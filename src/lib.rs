pub mod analyzer;
pub mod config;
pub mod evaluate;
pub mod model;
pub mod report;

use std::path::Path;

use analyzer::AnalysisOptions;
use anyhow::Context;
use config::LoadedConfig;
use model::ValidationReport;

pub fn validate(
    root: &Path,
    manifest_path: Option<&Path>,
    config_path: &Path,
    strict_override: bool,
) -> anyhow::Result<ValidationReport> {
    let config = LoadedConfig::load(config_path)
        .with_context(|| format!("could not load config {}", config_path.display()))?;
    let strict = strict_override || config.analysis.strict;
    let graph = analyzer::analyze(AnalysisOptions {
        root,
        manifest_path,
        strict,
    })?;

    Ok(evaluate::evaluate(graph, &config))
}
