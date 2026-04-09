//! Structured diagnostic types produced by the analyser and optimizer.
//!
//! Diagnostics are collected during a pipeline run and rendered either to the
//! terminal or to a machine-readable report file.  They are deliberately
//! separate from Rust errors: a [`Diagnostic`] is a *finding* about the
//! G-Code content, not a failure of the tool itself.

use std::fmt;

// ──────────────────────────────────────────────────────────────────────────────
// Severity
// ──────────────────────────────────────────────────────────────────────────────

/// The severity level of a diagnostic finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub enum Severity {
    /// Informational note; does not indicate a problem.
    Info,
    /// A potential issue that should be reviewed but does not prevent printing.
    Warning,
    /// A definite problem that will likely cause a failed or unsafe print.
    Error,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Info => write!(f, "info"),
            Self::Warning => write!(f, "warning"),
            Self::Error => write!(f, "error"),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Diagnostic
// ──────────────────────────────────────────────────────────────────────────────

/// A single finding produced by the analyser or optimizer.
///
/// Each diagnostic carries a stable [`code`][`Diagnostic::code`] (e.g.
/// `"E001"`) suitable for machine-readable output or CI filtering, plus a
/// human-readable [`message`][`Diagnostic::message`].
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Diagnostic {
    /// Severity of this finding.
    pub severity: Severity,

    /// 1-based source line number this finding refers to.
    pub line: u32,

    /// Stable, machine-readable diagnostic code (e.g. `"E001"`, `"W012"`).
    ///
    /// These codes are stable across versions and suitable for use in CI
    /// allow/deny lists.
    pub code: &'static str,

    /// Human-readable description of the finding.
    pub message: String,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}] line {}: {} — {}",
            self.severity, self.line, self.code, self.message
        )
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// PrintStats
// ──────────────────────────────────────────────────────────────────────────────

/// Statistics collected during a simulation/analysis pass.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PrintStats {
    /// Number of layer changes detected.
    pub layer_count: u32,

    /// Total XY+Z distance travelled by the print head in millimetres,
    /// including both printing and travel moves.
    pub total_distance_mm: f64,

    /// Total filament extruded in millimetres (E-axis delta in absolute mode).
    pub total_filament_mm: f64,

    /// Estimated print time in seconds, based on move distances and feed rates.
    ///
    /// This is a lower-bound estimate that ignores acceleration; actual print
    /// time will be longer.
    pub estimated_time_seconds: f64,

    /// Total number of move commands (G0 + G1) processed.
    pub move_count: usize,

    /// Minimum corner of the axis-aligned bounding box of all moves.
    pub bbox_min: crate::models::Point3D,

    /// Maximum corner of the axis-aligned bounding box of all moves.
    pub bbox_max: crate::models::Point3D,

    /// Estimated print time for each layer, in seconds.
    ///
    /// Populated during analysis; one entry per detected layer change.
    pub per_layer_times: Vec<f64>,
}

impl Default for PrintStats {
    fn default() -> Self {
        Self {
            layer_count: 0,
            total_distance_mm: 0.0,
            total_filament_mm: 0.0,
            estimated_time_seconds: 0.0,
            move_count: 0,
            bbox_min: crate::models::Point3D {
                x: f64::MAX,
                y: f64::MAX,
                z: f64::MAX,
            },
            bbox_max: crate::models::Point3D {
                x: f64::MIN,
                y: f64::MIN,
                z: f64::MIN,
            },
            per_layer_times: Vec::new(),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// OptimizationChange
// ──────────────────────────────────────────────────────────────────────────────

/// Records a single change made (or that would be made in dry-run) by the
/// optimizer.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct OptimizationChange {
    /// 1-based line number of the affected command.
    pub line: u32,

    /// Human-readable description of the change.
    pub description: String,
}

// ──────────────────────────────────────────────────────────────────────────────
// AnalysisReport
// ──────────────────────────────────────────────────────────────────────────────

/// The complete result of a pipeline run: diagnostics, statistics, and
/// optimizer changes.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AnalysisReport {
    /// All findings from the analyser and optimizer.
    pub diagnostics: Vec<Diagnostic>,

    /// Print statistics from the simulation pass.
    pub stats: PrintStats,

    /// Changes made or proposed by the optimizer.
    pub changes: Vec<OptimizationChange>,

    /// Whether this was a dry-run (no output file written).
    pub dry_run: bool,

    /// Slicer dialect and extracted metadata (if detection was run).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slicer: Option<crate::dialect::SlicerMetadata>,
}

impl AnalysisReport {
    /// Returns `true` if any diagnostic has [`Severity::Error`].
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }

    /// Returns the count of diagnostics at or above the given severity.
    #[must_use]
    pub fn count_at_least(&self, min: Severity) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity >= min)
            .count()
    }

    /// Writes a human-readable summary to the given writer.
    ///
    /// # Errors
    ///
    /// Returns an error if writing to `writer` fails.
    pub fn write_summary<W: fmt::Write>(&self, writer: &mut W) -> fmt::Result {
        writeln!(writer, "═══ GCode-Sentinel Report ═══")?;
        writeln!(writer, "Layers    : {}", self.stats.layer_count)?;
        writeln!(writer, "Moves     : {}", self.stats.move_count)?;
        writeln!(writer, "Distance  : {:.1} mm", self.stats.total_distance_mm)?;
        writeln!(writer, "Filament  : {:.1} mm", self.stats.total_filament_mm)?;
        let mins = self.stats.estimated_time_seconds / 60.0;
        writeln!(writer, "Est. time : {mins:.0} min")?;
        writeln!(writer, "Bbox min  : {}", self.stats.bbox_min)?;
        writeln!(writer, "Bbox max  : {}", self.stats.bbox_max)?;

        if let Some(ref slicer) = self.slicer {
            if slicer.dialect != crate::dialect::SlicerDialect::Unknown {
                writeln!(
                    writer,
                    "Slicer    : {:?} ({:?})",
                    slicer.dialect, slicer.confidence
                )?;
            }
            if let Some(ref v) = slicer.slicer_version {
                writeln!(writer, "Version   : {v}")?;
            }
        }

        if self.diagnostics.is_empty() {
            writeln!(writer, "\nNo issues found.")?;
        } else {
            writeln!(writer, "\nDiagnostics ({}):", self.diagnostics.len())?;
            for d in &self.diagnostics {
                writeln!(writer, "  {d}")?;
            }
        }

        if !self.changes.is_empty() {
            let label = if self.dry_run {
                "Would optimize"
            } else {
                "Optimized"
            };
            writeln!(writer, "\n{label} ({} changes):", self.changes.len())?;
            for c in &self.changes {
                writeln!(writer, "  line {}: {}", c.line, c.description)?;
            }
        }

        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// ValidationDiff
// ──────────────────────────────────────────────────────────────────────────────

/// Delta between pre- and post-optimization diagnostic sets.
///
/// Used to detect regressions introduced by the optimizer: if any new
/// [`Severity::Error`] diagnostic appears after optimization that was not
/// present before, [`regression_detected`][`ValidationDiff::regression_detected`]
/// is set to `true`.
#[derive(Debug, Clone)]
pub struct ValidationDiff {
    /// Error diagnostics present in post but absent from pre (matched by
    /// `code` + `line`).
    pub new_errors: Vec<Diagnostic>,

    /// Error diagnostics present in pre but absent from post.
    pub resolved_errors: Vec<Diagnostic>,

    /// `true` when [`new_errors`][`ValidationDiff::new_errors`] is non-empty.
    pub regression_detected: bool,
}

impl ValidationDiff {
    /// Compute the diff between `pre` and `post` diagnostic slices.
    ///
    /// Only [`Severity::Error`] diagnostics are considered for regression
    /// detection; warnings and info diagnostics are ignored.
    #[must_use]
    pub fn compute(pre: &[Diagnostic], post: &[Diagnostic]) -> Self {
        let pre_errors: Vec<&Diagnostic> = pre
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        let post_errors: Vec<&Diagnostic> = post
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();

        let new_errors: Vec<Diagnostic> = post_errors
            .iter()
            .filter(|p| {
                !pre_errors
                    .iter()
                    .any(|e| e.code == p.code && e.line == p.line)
            })
            .map(|d| (*d).clone())
            .collect();

        let resolved_errors: Vec<Diagnostic> = pre_errors
            .iter()
            .filter(|p| {
                !post_errors
                    .iter()
                    .any(|e| e.code == p.code && e.line == p.line)
            })
            .map(|d| (*d).clone())
            .collect();

        let regression_detected = !new_errors.is_empty();

        Self {
            new_errors,
            resolved_errors,
            regression_detected,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_serializes_to_string() {
        let s = serde_json::to_string(&Severity::Error).unwrap();
        assert_eq!(s, r#""Error""#);
    }

    #[test]
    fn analysis_report_serializes_to_json() {
        let report = AnalysisReport {
            diagnostics: vec![],
            stats: PrintStats::default(),
            changes: vec![],
            dry_run: false,
            slicer: None,
        };
        let json = serde_json::to_string(&report).unwrap();
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(val["stats"]["layer_count"].is_number());
    }

    #[test]
    fn validation_diff_detects_new_errors() {
        let pre = vec![Diagnostic {
            severity: Severity::Warning,
            line: 10,
            code: "W001",
            message: "existing warning".to_string(),
        }];
        let post = vec![
            Diagnostic {
                severity: Severity::Warning,
                line: 10,
                code: "W001",
                message: "existing warning".to_string(),
            },
            Diagnostic {
                severity: Severity::Error,
                line: 20,
                code: "E001",
                message: "new error".to_string(),
            },
        ];
        let diff = ValidationDiff::compute(&pre, &post);
        assert!(diff.regression_detected);
        assert_eq!(diff.new_errors.len(), 1);
        assert_eq!(diff.new_errors[0].code, "E001");
        assert!(diff.resolved_errors.is_empty());
    }

    #[test]
    fn validation_diff_no_regression_when_errors_unchanged() {
        let diags = vec![Diagnostic {
            severity: Severity::Error,
            line: 5,
            code: "E002",
            message: "pre-existing error".to_string(),
        }];
        let diff = ValidationDiff::compute(&diags, &diags);
        assert!(!diff.regression_detected);
        assert!(diff.new_errors.is_empty());
    }
}
