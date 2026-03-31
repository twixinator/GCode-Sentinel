//! Command-line interface definitions for GCode-Sentinel.
//!
//! This module defines the [`Cli`] struct parsed by [`clap`] via the derive
//! macro.  All arguments are declared here; orchestration logic lives in
//! `main.rs`.

use std::path::PathBuf;

use clap::Parser;
use clap::ValueEnum;

/// Output format for the analysis report written via `--report-file` or to
/// stdout when used without `--report-file`.
#[derive(Debug, Clone, Default, PartialEq, ValueEnum)]
pub enum ReportFormat {
    /// Human-readable text, identical to the stderr summary (default).
    #[default]
    Text,
    /// Machine-readable JSON.
    Json,
}

/// High-performance G-Code validator and optimizer for 3D printing.
// Boolean fields here are clap flags; each bool maps to exactly one CLI switch.
// Refactoring into enums would only add indirection without semantic benefit.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Parser)]
#[command(
    author,
    version,
    about = "High-performance G-Code validator and optimizer for 3D printing"
)]
pub struct Cli {
    /// Path to the input G-Code file to process.
    pub input: PathBuf,

    /// Output file path.
    ///
    /// When absent the processed output is written to stdout, or the tool
    /// derives a sibling path from the input filename (behaviour determined
    /// by the pipeline stage in `main.rs`).
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Path to an optional TOML configuration file containing machine
    /// profiles and per-machine overrides.
    #[arg(short, long)]
    pub config: Option<PathBuf>,

    /// Machine X-axis travel limit in millimetres.
    ///
    /// Overrides any value loaded from a config file.  If all three axis
    /// limits are absent and no config file is provided, out-of-bounds
    /// checking is skipped.
    #[arg(long)]
    pub max_x: Option<f64>,

    /// Machine Y-axis travel limit in millimetres.
    ///
    /// Overrides any value loaded from a config file.  If all three axis
    /// limits are absent and no config file is provided, out-of-bounds
    /// checking is skipped.
    #[arg(long)]
    pub max_y: Option<f64>,

    /// Machine Z-axis travel limit in millimetres.
    ///
    /// Overrides any value loaded from a config file.  If all three axis
    /// limits are absent and no config file is provided, out-of-bounds
    /// checking is skipped.
    #[arg(long)]
    pub max_z: Option<f64>,

    /// Run validation only — do not apply any optimizations.
    ///
    /// The tool exits with a non-zero status code if any errors are found.
    /// Useful as a CI gate or a pre-flight check before committing a G-Code
    /// file to a print queue.
    #[arg(long)]
    pub check_only: bool,

    /// Enable verbose/debug output.
    ///
    /// Prints per-command diagnostics, pipeline stage timings, and internal
    /// state transitions.  Intended for development and troubleshooting; not
    /// recommended for large files.
    #[arg(short, long)]
    pub verbose: bool,

    /// Write the analysis report to this file path in addition to the stderr
    /// summary.
    ///
    /// Format is controlled by `--report-format` (default: plain text).
    #[arg(long)]
    pub report_file: Option<PathBuf>,

    /// Format for the report produced by `--report-file`, or written to stdout
    /// when `--report-format json` is given without `--report-file`.
    ///
    /// When `json` is selected without `--report-file`, JSON is written to
    /// stdout and G-Code output is suppressed (implies check-only behaviour).
    #[arg(long, value_enum, default_value_t = ReportFormat::Text)]
    pub report_format: ReportFormat,

    /// Merge collinear consecutive linear moves into single moves.
    ///
    /// Detects three or more consecutive G1 commands on the same 3D line
    /// with consistent feedrate and proportional extrusion, replacing them
    /// with a single move.  Opt-in because it modifies move structure.
    #[arg(long)]
    pub merge_collinear: bool,

    /// Strip existing M73 progress markers and re-insert recalculated ones
    /// at each layer boundary.
    #[arg(long)]
    pub insert_progress: bool,

    /// Warn when any layer's estimated print time falls below this threshold.
    /// Disabled when absent.
    #[arg(long, value_name = "SECONDS")]
    pub min_layer_time: Option<f64>,

    /// Disable Rule 7 — consecutive same-axis travel merging.
    ///
    /// By default the optimizer removes an earlier single-axis non-extruding
    /// move when the very next move on the same axis (with the same feedrate)
    /// supersedes it.  Pass this flag to keep all intermediate travel commands.
    #[arg(long)]
    pub no_travel_merge: bool,

    /// Disable Rule 8 — redundant feedrate elimination.
    ///
    /// By default the optimizer strips the `F` parameter from moves whose
    /// feedrate already matches the current modal feedrate.  Pass this flag to
    /// preserve all feedrate annotations as-is.
    #[arg(long)]
    pub no_feedrate_strip: bool,

    /// Preserve slicer-computed M73 progress markers instead of stripping them.
    ///
    /// By default `--insert-progress` strips all existing M73 commands and
    /// inserts recalculated ones at every layer boundary.  When this flag is
    /// set, existing M73 commands are kept in place and new markers are only
    /// inserted at boundaries that do not already have one immediately
    /// preceding them.  Has no effect when `--insert-progress` is not set.
    #[arg(long)]
    pub trust_existing_m73: bool,

    /// Enable G2/G3 arc fitting optimization.
    ///
    /// Detects sequences of consecutive G1 moves that approximate a circular
    /// arc and replaces them with a single G2 (clockwise) or G3 (counter-
    /// clockwise) arc command.  Opt-in because it modifies command structure
    /// and requires firmware support for G2/G3.
    #[arg(long)]
    pub arc_fit: bool,

    /// Maximum radial deviation for arc fitting (mm).
    ///
    /// A G1 point must lie within this distance of the fitted circle to be
    /// included in an arc.  Default: 0.02 mm.
    #[arg(long, value_name = "MM")]
    pub arc_tolerance: Option<f64>,
}

impl Cli {
    /// Constructs a [`MachineLimits`] value from the axis-limit flags.
    ///
    /// Returns `Some` if at least one of `--max-x`, `--max-y`, or `--max-z`
    /// was supplied on the command line, allowing partial limit configurations
    /// (e.g. only the Z axis is constrained).  Returns `None` when all three
    /// are absent, signalling to the pipeline that out-of-bounds checking
    /// should be skipped or delegated to a config file.
    ///
    /// [`MachineLimits`]: crate::models::MachineLimits
    #[must_use]
    pub fn machine_limits(&self) -> Option<crate::models::MachineLimits> {
        if self.max_x.is_none() && self.max_y.is_none() && self.max_z.is_none() {
            return None;
        }
        let defaults = crate::models::MachineLimits::default();
        Some(crate::models::MachineLimits {
            max_x: self.max_x.unwrap_or(defaults.max_x),
            max_y: self.max_y.unwrap_or(defaults.max_y),
            max_z: self.max_z.unwrap_or(defaults.max_z),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn report_file_flag_is_parsed() {
        let cli = Cli::try_parse_from([
            "gcode-sentinel",
            "input.gcode",
            "--report-file",
            "/tmp/report.txt",
        ])
        .unwrap();
        assert_eq!(
            cli.report_file.unwrap().to_str().unwrap(),
            "/tmp/report.txt"
        );
    }

    #[test]
    fn report_format_json_is_parsed() {
        let cli = Cli::try_parse_from(["gcode-sentinel", "input.gcode", "--report-format", "json"])
            .unwrap();
        assert!(matches!(cli.report_format, ReportFormat::Json));
    }

    #[test]
    fn report_format_defaults_to_text() {
        let cli = Cli::try_parse_from(["gcode-sentinel", "input.gcode"]).unwrap();
        assert!(matches!(cli.report_format, ReportFormat::Text));
    }
}
