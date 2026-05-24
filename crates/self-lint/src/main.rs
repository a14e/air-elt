use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

mod checks;
mod diagnostic;
mod workspace;

use diagnostic::{Check, Diagnostic, Severity};

#[derive(Parser)]
#[command(
    name = "air-elt-self-lint",
    about = "Structural linter for the Air Elt workspace"
)]
struct Cli {
    #[arg(long, default_value = ".")]
    root: String,

    /// Skip specific checks by name (comma-separated)
    #[arg(long, value_delimiter = ',')]
    skip: Vec<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let root = match PathBuf::from(&cli.root).canonicalize() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("error: cannot resolve root '{}': {error}", cli.root);
            return ExitCode::FAILURE;
        }
    };

    let workspace = match workspace::Workspace::load(&root) {
        Ok(workspace) => workspace,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };

    let all_checks: Vec<Box<dyn Check>> = vec![
        Box::new(checks::ascii::NonEnglishCheck),
        Box::new(checks::dep_graph::DepGraphCheck),
        Box::new(checks::registry::RegistryCheck),
        Box::new(checks::traits::TraitsCheck),
        Box::new(checks::app_tests::AppTestsCheck),
        Box::new(checks::version::VersionCheck),
        Box::new(checks::containers::ContainersCheck),
        Box::new(checks::ci_env::CiEnvCheck),
        Box::new(checks::mod_purity::ModPurityCheck),
        Box::new(checks::test_aggregator::TestAggregatorCheck),
        Box::new(checks::doctest::DoctestCheck),
        Box::new(checks::worktree::WorktreeCheck),
    ];

    let mut diagnostics: Vec<Diagnostic> = all_checks
        .iter()
        .filter(|check| !cli.skip.iter().any(|skip| skip == check.name()))
        .flat_map(|check| {
            let name = check.name();
            check
                .run(&root, &workspace)
                .into_iter()
                .map(move |finding| Diagnostic::from_finding(finding, name))
        })
        .collect();

    diagnostics.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));

    for diagnostic in &diagnostics {
        eprintln!("{diagnostic}");
    }

    let error_count = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count();
    let warning_count = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .count();

    if error_count > 0 {
        eprintln!("\n{error_count} error(s), {warning_count} warning(s)");
        ExitCode::FAILURE
    } else if warning_count > 0 {
        eprintln!("\n{warning_count} warning(s)");
        ExitCode::SUCCESS
    } else {
        eprintln!("all structural checks passed");
        ExitCode::SUCCESS
    }
}
