#![warn(clippy::pedantic)]

use std::fs;
use std::io::Write as _;

use anyhow::{Context, Result};
use clap::Parser;
use memmap2::Mmap;
use tracing::{info, warn};

use gcode_sentinel::analyzer::analyze;
use gcode_sentinel::cli::Cli;
use gcode_sentinel::diagnostics::{AnalysisReport, OptimizationChange, Severity};
use gcode_sentinel::emitter::{emit, EmitConfig};
use gcode_sentinel::optimizer::{optimize, OptConfig};
use gcode_sentinel::parser::parse_all;

fn main() -> Result<()> {
    let cli = Cli::parse();

    init_tracing(cli.verbose)?;
    info!(input = %cli.input.display(), "starting GCode-Sentinel");

    validate_input(&cli)?;

    let limits = cli.machine_limits();
    log_limits(limits.as_ref());

    let text = map_input(&cli)?;
    let commands = parse_all(&text).context("parse error in input file")?;
    info!(commands = commands.len(), "parse complete");

    let analysis = analyze(commands.iter(), limits.as_ref());
    log_analysis(&analysis.diagnostics);

    let opt_config = OptConfig { dry_run: cli.check_only };
    let opt_result = optimize(commands, &opt_config);
    log_optimization(opt_result.changes.len(), cli.check_only);

    let report = build_report(analysis, opt_result.changes, cli.check_only);
    print_report(&report)?;

    if report.has_errors() && cli.check_only {
        anyhow::bail!(
            "validation failed: {} error(s) found",
            report.count_at_least(Severity::Error)
        );
    }

    if !cli.check_only {
        write_output(&cli, &opt_result.commands)?;
    }

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn init_tracing(verbose: bool) -> Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| {
            if verbose {
                tracing_subscriber::EnvFilter::new("debug")
            } else {
                tracing_subscriber::EnvFilter::new("info")
            }
        });
    let subscriber = tracing_subscriber::fmt().with_env_filter(filter).finish();
    tracing::subscriber::set_global_default(subscriber)
        .context("failed to initialise tracing subscriber")
}

fn validate_input(cli: &Cli) -> Result<()> {
    if !cli.input.exists() {
        anyhow::bail!("input file not found: {}", cli.input.display());
    }
    if !cli.input.is_file() {
        anyhow::bail!("input path is not a regular file: {}", cli.input.display());
    }
    Ok(())
}

fn log_limits(limits: Option<&gcode_sentinel::models::MachineLimits>) {
    if let Some(lim) = limits {
        info!(max_x = lim.max_x, max_y = lim.max_y, max_z = lim.max_z, "machine limits loaded");
    } else {
        info!("no machine limits provided — out-of-bounds checking disabled");
    }
}

/// Memory-maps the input file and validates UTF-8, returning an owned `String`.
///
/// An owned copy is necessary because the mmap lifetime does not extend beyond
/// this function; the parsed AST borrows from the returned `String`.
fn map_input(cli: &Cli) -> Result<String> {
    let file = fs::File::open(&cli.input)
        .with_context(|| format!("failed to open: {}", cli.input.display()))?;
    // SAFETY: file is read-only for this process; no external mutation assumed.
    let mmap = unsafe { Mmap::map(&file) }
        .with_context(|| format!("failed to memory-map: {}", cli.input.display()))?;
    info!(bytes = mmap.len(), path = %cli.input.display(), "file mapped");
    let text = std::str::from_utf8(&mmap)
        .with_context(|| format!("input is not valid UTF-8: {}", cli.input.display()))?
        .to_owned();
    Ok(text)
}

fn log_analysis(diagnostics: &[gcode_sentinel::diagnostics::Diagnostic]) {
    if diagnostics.is_empty() {
        info!("analysis complete — no issues found");
    } else {
        let errors = diagnostics.iter().filter(|d| d.severity == Severity::Error).count();
        let warnings = diagnostics.iter().filter(|d| d.severity == Severity::Warning).count();
        info!(errors, warnings, "analysis complete");
    }
}

fn log_optimization(change_count: usize, dry_run: bool) {
    if dry_run {
        info!(proposed_removals = change_count, "dry-run: no output written");
    } else {
        info!(removed = change_count, "optimization complete");
    }
}

fn build_report(
    analysis: gcode_sentinel::analyzer::AnalysisResult,
    changes: Vec<OptimizationChange>,
    dry_run: bool,
) -> AnalysisReport {
    AnalysisReport {
        diagnostics: analysis.diagnostics,
        stats: analysis.stats,
        changes,
        dry_run,
    }
}

fn print_report(report: &AnalysisReport) -> Result<()> {
    let mut summary = String::new();
    report.write_summary(&mut summary).context("failed to format report")?;
    eprintln!("{summary}");
    if report.has_errors() {
        warn!(
            error_count = report.count_at_least(Severity::Error),
            "errors found in input file"
        );
    }
    Ok(())
}

fn write_output(
    cli: &Cli,
    commands: &[gcode_sentinel::models::Spanned<gcode_sentinel::models::GCodeCommand<'_>>],
) -> Result<()> {
    let config = EmitConfig::default();
    if let Some(ref path) = cli.output {
        let mut file = fs::File::create(path)
            .with_context(|| format!("failed to create output: {}", path.display()))?;
        emit(commands, &mut file, &config).context("failed to write G-Code")?;
        info!(output = %path.display(), "output written");
    } else {
        let stdout = std::io::stdout();
        let mut handle = stdout.lock();
        emit(commands, &mut handle, &config).context("failed to write G-Code to stdout")?;
        handle.flush().context("failed to flush stdout")?;
    }
    Ok(())
}
