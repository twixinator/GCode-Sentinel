//! Command-line interface definitions for GCode-Sentinel.
//!
//! This module defines the [`Cli`] struct parsed by [`clap`] via the derive
//! macro.  All arguments are declared here; orchestration logic lives in
//! `main.rs`.

use std::path::PathBuf;

use clap::Parser;
use clap::ValueEnum;

/// Slicer dialect for `--dialect` override.
#[derive(Debug, Clone, Copy, PartialEq, ValueEnum)]
pub enum CliDialect {
    OrcaSlicer,
    PrusaSlicer,
    Cura,
}

impl CliDialect {
    #[must_use]
    pub fn to_slicer_dialect(self) -> crate::dialect::SlicerDialect {
        match self {
            Self::OrcaSlicer => crate::dialect::SlicerDialect::OrcaSlicer,
            Self::PrusaSlicer => crate::dialect::SlicerDialect::PrusaSlicer,
            Self::Cura => crate::dialect::SlicerDialect::Cura,
        }
    }
}

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

    /// Select a built-in machine profile by name (e.g. `ender3`, `prusa_mk4`).
    ///
    /// The profile sets default axis limits and optional firmware metadata.
    /// Explicit `--max-x/y/z` flags override the profile values.
    /// Use `--machine help` (or supply an invalid name) to see all available
    /// profiles listed in the error message.
    ///
    /// Resolution order (later overrides earlier):
    /// defaults → `--config` file → `--machine` profile → explicit flags.
    #[arg(long, value_name = "PROFILE")]
    pub machine: Option<String>,

    /// Override auto-detected slicer dialect.
    #[arg(long, value_enum, value_name = "DIALECT")]
    pub dialect: Option<CliDialect>,
}

impl Cli {
    /// Constructs a [`MachineLimits`] value following the resolution order.
    ///
    /// Resolution order (later source overrides earlier):
    /// 1. No limits at all (returns `None` when nothing is configured).
    /// 2. A `--machine` profile supplied via `profile` (axis bounds only).
    /// 3. Explicit `--max-x`, `--max-y`, `--max-z` flags on the command line.
    ///
    /// Returns `Some` when either a profile is given **or** at least one
    /// explicit axis flag is present.  Returns `None` when both are absent,
    /// signalling to the pipeline that out-of-bounds checking should be skipped.
    ///
    /// # Examples
    ///
    /// ```
    /// use gcode_sentinel::cli::Cli;
    /// use clap::Parser;
    ///
    /// let cli = Cli::try_parse_from(["gcode-sentinel", "in.gcode", "--max-x", "220"]).unwrap();
    /// let limits = cli.machine_limits(None);
    /// assert_eq!(limits.unwrap().max_x, 220.0);
    /// ```
    ///
    /// [`MachineLimits`]: crate::models::MachineLimits
    #[must_use]
    pub fn machine_limits(
        &self,
        profile: Option<&crate::machine_profile::MachineProfile>,
    ) -> Option<crate::models::MachineLimits> {
        let has_explicit = self.max_x.is_some() || self.max_y.is_some() || self.max_z.is_some();

        // Profile base — if a profile was loaded, use it as the starting point.
        let base: Option<crate::models::MachineLimits> =
            profile.map(crate::machine_profile::MachineProfile::to_machine_limits);

        if base.is_none() && !has_explicit {
            // Nothing configured — disable out-of-bounds checking.
            return None;
        }

        // Start from profile limits when available; fall back to type defaults
        // only if an explicit flag is present without a profile.
        let base = base.unwrap_or_default();

        Some(crate::models::MachineLimits {
            max_x: self.max_x.unwrap_or(base.max_x),
            max_y: self.max_y.unwrap_or(base.max_y),
            max_z: self.max_z.unwrap_or(base.max_z),
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

    // ── --machine flag ───────────────────────────────────────────────────────

    #[test]
    fn test_machine_flag_parsed_correctly() {
        let cli =
            Cli::try_parse_from(["gcode-sentinel", "input.gcode", "--machine", "ender3"]).unwrap();
        assert_eq!(cli.machine.as_deref(), Some("ender3"));
    }

    #[test]
    fn test_machine_flag_absent_is_none() {
        let cli = Cli::try_parse_from(["gcode-sentinel", "input.gcode"]).unwrap();
        assert_eq!(cli.machine, None);
    }

    // ── machine_limits resolution order ─────────────────────────────────────

    #[test]
    fn test_machine_limits_no_profile_no_flags_returns_none() {
        let cli = Cli::try_parse_from(["gcode-sentinel", "input.gcode"]).unwrap();
        assert!(cli.machine_limits(None).is_none());
    }

    #[test]
    fn test_machine_limits_profile_only_uses_profile_values() {
        use crate::machine_profile::load_profile;
        let cli = Cli::try_parse_from(["gcode-sentinel", "input.gcode"]).unwrap();
        let profile = load_profile("ender3").unwrap();
        let limits = cli.machine_limits(Some(&profile)).unwrap();
        assert_eq!(limits.max_x, 220.0);
        assert_eq!(limits.max_y, 220.0);
        assert_eq!(limits.max_z, 250.0);
    }

    #[test]
    fn test_machine_limits_explicit_flags_override_profile() {
        use crate::machine_profile::load_profile;
        let cli = Cli::try_parse_from([
            "gcode-sentinel",
            "input.gcode",
            "--max-x",
            "180.0",
            "--max-z",
            "300.0",
        ])
        .unwrap();
        let profile = load_profile("ender3").unwrap(); // ender3: 220 × 220 × 250
        let limits = cli.machine_limits(Some(&profile)).unwrap();
        // Explicit --max-x and --max-z override the profile; --max-y falls back to profile.
        assert_eq!(
            limits.max_x, 180.0,
            "explicit --max-x must win over profile"
        );
        assert_eq!(limits.max_y, 220.0, "--max-y absent: profile value used");
        assert_eq!(
            limits.max_z, 300.0,
            "explicit --max-z must win over profile"
        );
    }

    #[test]
    fn test_machine_limits_explicit_flags_only_no_profile() {
        let cli = Cli::try_parse_from([
            "gcode-sentinel",
            "input.gcode",
            "--max-x",
            "400.0",
            "--max-y",
            "400.0",
            "--max-z",
            "500.0",
        ])
        .unwrap();
        let limits = cli.machine_limits(None).unwrap();
        assert_eq!(limits.max_x, 400.0);
        assert_eq!(limits.max_y, 400.0);
        assert_eq!(limits.max_z, 500.0);
    }

    #[test]
    fn test_machine_limits_single_explicit_flag_with_profile_partial_override() {
        use crate::machine_profile::load_profile;
        let cli =
            Cli::try_parse_from(["gcode-sentinel", "input.gcode", "--max-y", "150.0"]).unwrap();
        let profile = load_profile("voron_v2").unwrap(); // voron: 350 × 350 × 350
        let limits = cli.machine_limits(Some(&profile)).unwrap();
        assert_eq!(limits.max_x, 350.0, "x falls back to profile");
        assert_eq!(limits.max_y, 150.0, "explicit --max-y overrides profile");
        assert_eq!(limits.max_z, 350.0, "z falls back to profile");
    }
}
