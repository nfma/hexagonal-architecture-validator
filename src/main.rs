use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use hexagonal_architecture_validator::model::Outcome;

#[derive(Debug, Parser)]
#[command(name = "hav", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Validate declared dependency boundaries.
    Check(CheckArgs),
}

#[derive(Debug, clap::Args)]
struct CheckArgs {
    /// Project or workspace directory to analyze.
    #[arg(long, default_value = ".")]
    root: PathBuf,

    /// Cargo manifest, relative to --root unless absolute.
    #[arg(long)]
    manifest_path: Option<PathBuf>,

    /// Validator config, relative to --root unless absolute.
    #[arg(long, default_value = "hav.toml")]
    config: PathBuf,

    /// Report format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,

    /// Fail on module-level macros that cannot be expanded.
    #[arg(long)]
    strict: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Check(args) => check(args),
    }
}

fn check(args: CheckArgs) -> ExitCode {
    let config = if args.config.is_absolute() {
        args.config
    } else {
        args.root.join(args.config)
    };
    let report = match hexagonal_architecture_validator::validate(
        &args.root,
        args.manifest_path.as_deref(),
        &config,
        args.strict,
    ) {
        Ok(report) => report,
        Err(error) => {
            let message = format!("{error:#}");
            match args.format {
                OutputFormat::Text => {
                    eprintln!("configuration or analysis error: {message}");
                }
                OutputFormat::Json => {
                    match hexagonal_architecture_validator::report::render_error_json(&message) {
                        Ok(output) => print!("{output}"),
                        Err(render_error) => {
                            eprintln!("could not render error report: {render_error:#}");
                        }
                    }
                }
            }
            return ExitCode::from(2);
        }
    };

    let rendered = match args.format {
        OutputFormat::Text => Ok(hexagonal_architecture_validator::report::render_text(
            &report,
        )),
        OutputFormat::Json => hexagonal_architecture_validator::report::render_json(&report),
    };
    match rendered {
        Ok(output) => print!("{output}"),
        Err(error) => {
            eprintln!("could not render report: {error:#}");
            return ExitCode::from(2);
        }
    }

    match report.outcome {
        Outcome::Passed => ExitCode::SUCCESS,
        Outcome::Violations => ExitCode::from(1),
        Outcome::AnalysisFailure => ExitCode::from(2),
    }
}
