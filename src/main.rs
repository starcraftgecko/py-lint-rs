mod diagnostics;
mod fix;
mod line_index;
mod rules;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use colored::Colorize;
use walkdir::WalkDir;

use line_index::LineIndex;

/// A fast Python linter written in Rust.
#[derive(Parser, Debug)]
#[command(name = "pylint-rs", version, about)]
struct Cli {
    /// Files or directories to lint. Defaults to the current directory.
    #[arg(default_value = ".")]
    paths: Vec<PathBuf>,

    /// Remove fully-unused import statements (F401 only). Prints a diff of
    /// what will be removed before writing the file. Only whole, single-line
    /// import statements where every bound name is unused are touched;
    /// partially-unused or multi-line imports are left for manual review.
    #[arg(long)]
    fix: bool,
}

fn collect_python_files(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_file() {
            files.push(path.clone());
            continue;
        }
        for entry in WalkDir::new(path)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
        {
            if entry.path().extension().and_then(|e| e.to_str()) == Some("py") {
                files.push(entry.path().to_path_buf());
            }
        }
    }
    files
}

fn lint_file(path: &Path, apply_fix: bool) -> anyhow::Result<Vec<diagnostics::Diagnostic>> {
    let mut source = std::fs::read_to_string(path)?;

    if apply_fix {
        let line_index = LineIndex::new(&source);
        let fixes = fix::find_unused_import_fixes(&source, &line_index)?;
        if !fixes.is_empty() {
            println!("{}", format!("--- {}", path.display()).bold());
            print!("{}", fix::render_diff(&source, &fixes));
            source = fix::apply_fixes(&source, &fixes);
            std::fs::write(path, &source)?;
            println!(
                "{}",
                format!(
                    "applied {} fix{}",
                    fixes.len(),
                    if fixes.len() == 1 { "" } else { "es" }
                )
                .green()
            );
        }
    }

    let line_index = LineIndex::new(&source);
    rules::check(path, &source, &line_index)
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let files = collect_python_files(&cli.paths);

    if files.is_empty() {
        eprintln!("{}", "no Python files found".yellow());
        return ExitCode::SUCCESS;
    }

    let mut total = 0usize;
    let mut had_error = false;

    for file in &files {
        match lint_file(file, cli.fix) {
            Ok(diags) => {
                total += diags.len();
                for diag in diags {
                    println!(
                        "{} {}",
                        format!("{}:{}:{}:", diag.file.display(), diag.line, diag.col).cyan(),
                        format!("{} {}", diag.code.red().bold(), diag.message)
                    );
                }
            }
            Err(e) => {
                had_error = true;
                eprintln!("{} {}: {}", "error:".red().bold(), file.display(), e);
            }
        }
    }

    println!();
    if total == 0 {
        println!("{}", "All checks passed!".green().bold());
    } else {
        println!(
            "{}",
            format!("Found {total} issue{}", if total == 1 { "" } else { "s" })
                .red()
                .bold()
        );
    }

    if had_error {
        ExitCode::from(2)
    } else if total > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
