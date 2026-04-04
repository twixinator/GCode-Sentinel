#![warn(clippy::pedantic)]

use std::fs;
use std::io::Write as IoWrite;

use anyhow::{Context, Result};
use clap::Parser;
use memmap2::Mmap;
use tracing::{info, warn};

use gcode_sentinel::analyzer::analyze;
use gcode_sentinel::arc_fitter::{fit_arcs, ArcFitConfig, DEFAULT_ARC_TOLERANCE_MM};
use gcode_sentinel::cli::{Cli, ReportFormat};
use gcode_sentinel::diagnostics::{AnalysisReport, OptimizationChange, Severity, ValidationDiff};
use gcode_sentinel::emitter::{emit, EmitConfig};
use gcode_sentinel::machine_profile::{self, MachineProfile};
use gcode_sentinel::optimizer::{optimize, OptConfig};
use gcode_sentinel::parser::parse_all;

fn main() -> Result<()> {
    let cli = Cli::parse();

    init_tracing(cli.verbose)?;
    info!(input = %cli.input.display(), "starting GCode-Sentinel");

    validate_input(&cli)?;
    validate_cli_flags(&cli)?;

    let profile = resolve_machine_profile(&cli)?;
    let limits = cli.machine_limits(profile.as_ref());
    log_limits(limits.as_ref());

    let text = map_input(&cli)?;
    let commands = parse_all(&text).context("parse error in input file")?;
    info!(commands = commands.len(), "parse complete");

    let pre_analysis = analyze(commands.iter(), limits.as_ref());
    log_analysis(&pre_analysis.diagnostics);

    // JSON-to-stdout without --report-file implies check-only (no G-Code written).
    let effective_dry_run =
        cli.check_only || (cli.report_format == ReportFormat::Json && cli.report_file.is_none());
    let opt_config = OptConfig {
        dry_run: effective_dry_run,
        merge_collinear: cli.merge_collinear,
        insert_progress: cli.insert_progress,
        no_travel_merge: cli.no_travel_merge,
        no_feedrate_strip: cli.no_feedrate_strip,
        trust_existing_m73: cli.trust_existing_m73,
    };

    // Pre-pass: collinear merge (opt-in).
    let merge_result = gcode_sentinel::optimizer::merge_collinear(commands, &opt_config);
    let mut all_changes = merge_result.changes;

    // Pre-pass: arc fitting (opt-in).
    let arc_config = ArcFitConfig {
        enabled: cli.arc_fit,
        tolerance_mm: cli.arc_tolerance.unwrap_or(DEFAULT_ARC_TOLERANCE_MM),
    };
    // Validate config before running fit_arcs so that a misconfigured tolerance
    // (zero, negative, NaN, infinite) fails fast with a clear message instead of
    // silently producing corrupt arc commands via broken float comparisons.
    if let Err(e) = arc_config.validate() {
        anyhow::bail!(e);
    }
    let arc_result = fit_arcs(merge_result.commands, &arc_config);
    all_changes.extend(arc_result.changes);

    // Main optimizer pass.
    let opt_result = optimize(arc_result.commands, &opt_config);
    all_changes.extend(opt_result.changes);
    log_optimization(all_changes.len(), effective_dry_run);

    // Post-pass: M73 progress markers (opt-in).
    let progress_result = gcode_sentinel::optimizer::insert_progress_markers(
        opt_result.commands,
        pre_analysis.stats.estimated_time_seconds,
        pre_analysis.stats.layer_count,
        &pre_analysis.stats.per_layer_times,
        &opt_config,
    );

    // Re-analyze the final command list to detect regressions.
    let post_analysis = analyze(progress_result.commands.iter(), limits.as_ref());
    let diff = ValidationDiff::compute(&pre_analysis.diagnostics, &post_analysis.diagnostics);
    if diff.regression_detected {
        for e in &diff.new_errors {
            warn!(
                code = e.code,
                line = e.line,
                message = %e.message,
                "optimizer introduced new error"
            );
        }
        anyhow::bail!(
            "optimizer regression: {} new error(s) appeared after optimization",
            diff.new_errors.len()
        );
    }

    let mut report = build_report(post_analysis, all_changes, effective_dry_run);

    // Add arc fitting diagnostics (e.g. W004 firmware warning).
    report.diagnostics.extend(arc_result.diagnostics);
    // Add progress insertion diagnostics.
    report.diagnostics.extend(progress_result.diagnostics);

    // Minimum layer time advisory (W003).
    if let Some(threshold) = cli.min_layer_time {
        for (i, &layer_time) in report.stats.per_layer_times.iter().enumerate() {
            if layer_time < threshold {
                report
                    .diagnostics
                    .push(gcode_sentinel::diagnostics::Diagnostic {
                        severity: Severity::Warning,
                        line: 0,
                        code: "W003",
                        message: format!(
                            "layer {} print time {layer_time:.1}s is below minimum {threshold}s",
                            i + 1,
                        ),
                    });
            }
        }
    }

    print_report(&report)?;
    write_report_file(&cli, &report)?;

    // JSON-to-stdout mode: write JSON, skip G-Code output.
    if cli.report_format == ReportFormat::Json && cli.report_file.is_none() {
        let json =
            serde_json::to_string_pretty(&report).context("failed to serialize report to JSON")?;
        println!("{json}");
        return Ok(());
    }

    if report.has_errors() && cli.check_only {
        anyhow::bail!(
            "validation failed: {} error(s) found",
            report.count_at_least(Severity::Error)
        );
    }

    if !effective_dry_run {
        write_output(&cli, &progress_result.commands)?;
    }

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Loads the machine profile selected via `--machine`, if any.
///
/// Returns `Ok(None)` when the flag is absent.  Returns a descriptive error
/// (listing all valid names) when an unknown profile name is supplied.
fn resolve_machine_profile(cli: &Cli) -> Result<Option<MachineProfile>> {
    let Some(ref name) = cli.machine else {
        return Ok(None);
    };
    machine_profile::load_profile(name)
        .map(Some)
        .with_context(|| format!("failed to load machine profile '{name}'"))
}

fn init_tracing(verbose: bool) -> Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
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

fn validate_cli_flags(cli: &Cli) -> Result<()> {
    if cli.report_format == ReportFormat::Json && cli.report_file.is_none() && cli.output.is_some()
    {
        anyhow::bail!(
            "--report-format json without --report-file cannot be combined with --output \
             (JSON would go to stdout, conflicting with the G-Code output destination)"
        );
    }
    Ok(())
}

fn log_limits(limits: Option<&gcode_sentinel::models::MachineLimits>) {
    if let Some(lim) = limits {
        info!(
            max_x = lim.max_x,
            max_y = lim.max_y,
            max_z = lim.max_z,
            "machine limits loaded"
        );
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
        let errors = diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count();
        let warnings = diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .count();
        info!(errors, warnings, "analysis complete");
    }
}

fn log_optimization(change_count: usize, dry_run: bool) {
    if dry_run {
        info!(
            proposed_removals = change_count,
            "dry-run: no output written"
        );
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
    report
        .write_summary(&mut summary)
        .context("failed to format report")?;
    eprintln!("{summary}");
    if report.has_errors() {
        warn!(
            error_count = report.count_at_least(Severity::Error),
            "errors found in input file"
        );
    }
    Ok(())
}

fn write_report_file(cli: &Cli, report: &AnalysisReport) -> Result<()> {
    let Some(ref path) = cli.report_file else {
        return Ok(());
    };
    let mut file = fs::File::create(path)
        .with_context(|| format!("failed to create report file: {}", path.display()))?;
    match cli.report_format {
        ReportFormat::Text => {
            let mut summary = String::new();
            report
                .write_summary(&mut summary)
                .context("failed to format text report")?;
            file.write_all(summary.as_bytes())
                .context("failed to write text report file")?;
        }
        ReportFormat::Json => {
            serde_json::to_writer_pretty(&mut file, report)
                .context("failed to write JSON report file")?;
        }
    }
    info!(path = %path.display(), format = ?cli.report_format, "report file written");
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
