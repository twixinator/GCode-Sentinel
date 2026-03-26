//! Virtual print-head simulator and G-Code analyser.
//!
//! This module performs a **read-only** single pass over a parsed G-Code AST,
//! simulating the motion of a print head to produce:
//!
//! * [`PrintStats`] — aggregate statistics (layer count, distance, filament,
//!   estimated time, bounding box).
//! * [`Vec<Diagnostic>`] — structured findings (out-of-bounds moves, negative
//!   coordinates, retraction events, layer changes).
//!
//! The entry point is [`analyze`].  It is a pure function with no I/O or side
//! effects; all tracing/logging is the caller's responsibility.

use crate::{
    diagnostics::{Diagnostic, PrintStats, Severity},
    models::{GCodeCommand, MachineLimits, Point3D, Spanned},
};

// ─────────────────────────────────────────────────────────────────────────────
// Public types
// ─────────────────────────────────────────────────────────────────────────────

/// The result of a full analysis pass over a G-Code AST.
#[derive(Debug, Clone)]
pub struct AnalysisResult {
    /// All findings produced during the simulation pass.
    pub diagnostics: Vec<Diagnostic>,
    /// Aggregate print statistics gathered during simulation.
    pub stats: PrintStats,
}

// ─────────────────────────────────────────────────────────────────────────────
// Private state
// ─────────────────────────────────────────────────────────────────────────────

/// Mutable simulation state threaded through the analyser.
///
/// This is an internal implementation detail; callers only see [`AnalysisResult`].
struct PrinterState {
    /// Current tool position in absolute machine coordinates (mm).
    pos: Point3D,
    /// Current extruder logical position (mm).
    extruder: f64,
    /// Current feed rate (mm/min).  Defaults to 1 500 mm/min if never set.
    feedrate: f64,
    /// `true` after G90 (absolute), `false` after G91 (relative).
    is_absolute: bool,
    /// Extruder positioning follows `is_absolute` unless overridden by a
    /// slicer-specific command.
    e_absolute: bool,
    /// Last Z seen during a linear move; used for Z-based layer detection.
    last_z: f64,
}

impl Default for PrinterState {
    fn default() -> Self {
        Self {
            pos: Point3D::default(),
            extruder: 0.0,
            feedrate: 1_500.0,
            is_absolute: true,
            e_absolute: true,
            last_z: 0.0,
        }
    }
}

/// Axis parameters parsed from a G0 or G1 command.
///
/// Bundling these reduces the number of single-character parameters passed
/// individually to internal helpers, satisfying `clippy::many_single_char_names`
/// and `clippy::too_many_arguments`.
struct AxisParams {
    /// Target X coordinate, if specified.
    target_x: Option<f64>,
    /// Target Y coordinate, if specified.
    target_y: Option<f64>,
    /// Target Z coordinate, if specified.
    target_z: Option<f64>,
    /// Extruder position/delta, if specified.
    extruder_e: Option<f64>,
    /// Feed rate override, if specified.
    feedrate_f: Option<f64>,
}

/// Mutable output targets passed into move handlers.
///
/// Bundling these reduces the argument count on internal functions.
struct MoveOutputs<'o> {
    /// Printer simulation state to update in place.
    printer: &'o mut PrinterState,
    /// Diagnostics list to append to.
    diags: &'o mut Vec<Diagnostic>,
    /// Statistics to accumulate into.
    print_stats: &'o mut PrintStats,
}

// ─────────────────────────────────────────────────────────────────────────────
// Public API
// ─────────────────────────────────────────────────────────────────────────────

/// Run the analyser over an iterator of spanned G-Code commands.
///
/// The `limits` parameter is optional: pass `Some(&limits)` to enable
/// out-of-bounds checking (diagnostic codes `E001`–`E003`), or `None` to skip
/// it when the user has not supplied machine dimensions.
///
/// The function is **pure**: it allocates no I/O resources, emits no log
/// records, and has no observable side effects beyond returning the result.
///
/// # Example
///
/// ```rust
/// use gcode_sentinel::analyzer::analyze;
/// use gcode_sentinel::models::{GCodeCommand, MachineLimits, Spanned};
///
/// let commands = vec![
///     Spanned { inner: GCodeCommand::SetAbsolute, line: 1, byte_offset: 0 },
///     Spanned {
///         inner: GCodeCommand::LinearMove { x: Some(50.0), y: Some(50.0), z: None, e: Some(1.0), f: Some(3000.0) },
///         line: 2,
///         byte_offset: 10,
///     },
/// ];
/// let limits = MachineLimits { max_x: 300.0, max_y: 300.0, max_z: 400.0 };
/// let result = analyze(commands.iter(), Some(&limits));
/// assert_eq!(result.stats.move_count, 1);
/// assert!(result.diagnostics.is_empty());
/// ```
#[must_use]
pub fn analyze<'a>(
    commands: impl Iterator<Item = &'a Spanned<GCodeCommand<'a>>>,
    limits: Option<&MachineLimits>,
) -> AnalysisResult {
    let mut printer = PrinterState::default();
    let mut diags: Vec<Diagnostic> = Vec::new();
    let mut print_stats = PrintStats::default();

    for spanned in commands {
        let mut outputs = MoveOutputs {
            printer: &mut printer,
            diags: &mut diags,
            print_stats: &mut print_stats,
        };
        process_command(spanned, &mut outputs, limits);
    }

    AnalysisResult {
        diagnostics: diags,
        stats: print_stats,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Command dispatch
// ─────────────────────────────────────────────────────────────────────────────

/// Dispatch a single spanned command to the appropriate handler.
fn process_command<'a>(
    spanned: &'a Spanned<GCodeCommand<'a>>,
    outputs: &mut MoveOutputs<'_>,
    limits: Option<&MachineLimits>,
) {
    match &spanned.inner {
        GCodeCommand::RapidMove {
            x,
            y,
            z,
            f,
        } => {
            let params = AxisParams {
                target_x: *x,
                target_y: *y,
                target_z: *z,
                extruder_e: None,
                feedrate_f: *f,
            };
            handle_move(spanned.line, &params, outputs, limits);
        }
        GCodeCommand::LinearMove { x, y, z, e, f } => {
            let params = AxisParams {
                target_x: *x,
                target_y: *y,
                target_z: *z,
                extruder_e: *e,
                feedrate_f: *f,
            };
            handle_linear_move(spanned.line, &params, outputs, limits);
        }
        GCodeCommand::SetAbsolute => {
            outputs.printer.is_absolute = true;
            outputs.printer.e_absolute = true;
        }
        GCodeCommand::SetRelative => {
            outputs.printer.is_absolute = false;
            outputs.printer.e_absolute = false;
        }
        GCodeCommand::SetPosition { x, y, z, e } => {
            handle_set_position(*x, *y, *z, *e, outputs.printer);
        }
        GCodeCommand::Comment { text } => {
            handle_comment(text, outputs.print_stats);
        }
        // GCommand, MetaCommand, Unknown: no motion semantics — skip.
        GCodeCommand::GCommand { .. }
        | GCodeCommand::MetaCommand { .. }
        | GCodeCommand::Unknown { .. } => {}
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// G0 / G1 motion handlers
// ─────────────────────────────────────────────────────────────────────────────

/// Handle a G0 (rapid, non-extruding) move.
fn handle_move(
    line: u32,
    params: &AxisParams,
    outputs: &mut MoveOutputs<'_>,
    limits: Option<&MachineLimits>,
) {
    if let Some(feed) = params.feedrate_f {
        outputs.printer.feedrate = feed;
    }

    let dest = resolve_destination(params, outputs.printer);

    emit_negative_coord_warning(line, &dest, outputs.diags);
    if let Some(lim) = limits {
        emit_bounds_errors(line, &dest, lim, outputs.diags);
    }

    let travel = euclidean_distance(&outputs.printer.pos, &dest);
    update_move_stats(travel, outputs.printer.feedrate, outputs.print_stats);
    update_bbox(&dest, outputs.print_stats);

    outputs.printer.pos = dest;
}

/// Handle a G1 (linear) move, which may include extrusion and layer detection.
fn handle_linear_move(
    line: u32,
    params: &AxisParams,
    outputs: &mut MoveOutputs<'_>,
    limits: Option<&MachineLimits>,
) {
    if let Some(feed) = params.feedrate_f {
        outputs.printer.feedrate = feed;
    }

    let dest = resolve_destination(params, outputs.printer);

    emit_negative_coord_warning(line, &dest, outputs.diags);
    if let Some(lim) = limits {
        emit_bounds_errors(line, &dest, lim, outputs.diags);
    }

    // Extruder delta and retraction diagnostics.
    if let Some(e_delta) = compute_e_delta(params.extruder_e, outputs.printer) {
        emit_retraction_diagnostic(line, e_delta, outputs.diags);
        if e_delta > 0.0 {
            outputs.print_stats.total_filament_mm += e_delta;
        }
        // Commit the new extruder logical position.
        if outputs.printer.e_absolute {
            if let Some(raw_e) = params.extruder_e {
                outputs.printer.extruder = raw_e;
            }
        } else {
            outputs.printer.extruder += params.extruder_e.unwrap_or(0.0);
        }
    }

    // Z-based layer change detection: Z increases while a LinearMove specifies Z.
    // `OrcaSlicer`'s `;LAYER_CHANGE` comment is the preferred signal; this is the
    // fallback for slicers that do not emit it.
    if params.target_z.is_some() && dest.z > outputs.printer.last_z {
        outputs.print_stats.layer_count += 1;
        outputs.diags.push(Diagnostic {
            severity: Severity::Info,
            line,
            code: "I001",
            message: format!(
                "layer change detected: Z {:.3} → {:.3}",
                outputs.printer.last_z,
                dest.z,
            ),
        });
        outputs.printer.last_z = dest.z;
    }

    let travel = euclidean_distance(&outputs.printer.pos, &dest);
    update_move_stats(travel, outputs.printer.feedrate, outputs.print_stats);
    update_bbox(&dest, outputs.print_stats);

    outputs.printer.pos = dest;
}

// ─────────────────────────────────────────────────────────────────────────────
// G92 – Set logical position
// ─────────────────────────────────────────────────────────────────────────────

/// Apply a G92 `SetPosition` command: update logical coordinates without moving.
///
/// This is critical for correct filament accounting — slicers commonly emit
/// `G92 E0` to reset the extruder counter at the start of each layer.
fn handle_set_position(
    set_x: Option<f64>,
    set_y: Option<f64>,
    set_z: Option<f64>,
    set_e: Option<f64>,
    printer: &mut PrinterState,
) {
    if let Some(val) = set_x {
        printer.pos.x = val;
    }
    if let Some(val) = set_y {
        printer.pos.y = val;
    }
    if let Some(val) = set_z {
        printer.pos.z = val;
    }
    if let Some(val) = set_e {
        printer.extruder = val;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Comment handling (OrcaSlicer layer-change signal)
// ─────────────────────────────────────────────────────────────────────────────

/// Handle a `Comment` command.
///
/// `OrcaSlicer` emits `;LAYER_CHANGE` as a dedicated comment.  This is more
/// reliable than Z-based detection because it fires even when a Z move is split
/// across multiple commands.  When we see it we increment the layer count
/// directly without re-emitting an `I001` diagnostic (the comment itself is the
/// ground truth; the Z-based diagnostic remains for slicers that do not emit
/// the comment).
fn handle_comment<S: AsRef<str>>(text: S, print_stats: &mut PrintStats) {
    if text.as_ref() == "LAYER_CHANGE" {
        print_stats.layer_count += 1;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Coordinate resolution helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Resolve axis parameters into an absolute destination point.
///
/// In absolute mode, absent axes retain their current value.
/// In relative mode, absent axes contribute a delta of zero.
fn resolve_destination(params: &AxisParams, printer: &PrinterState) -> Point3D {
    if printer.is_absolute {
        Point3D {
            x: params.target_x.unwrap_or(printer.pos.x),
            y: params.target_y.unwrap_or(printer.pos.y),
            z: params.target_z.unwrap_or(printer.pos.z),
        }
    } else {
        Point3D {
            x: printer.pos.x + params.target_x.unwrap_or(0.0),
            y: printer.pos.y + params.target_y.unwrap_or(0.0),
            z: printer.pos.z + params.target_z.unwrap_or(0.0),
        }
    }
}

/// Compute the extruder delta for a move, returning `None` when no E parameter
/// was present.
fn compute_e_delta(raw_e: Option<f64>, printer: &PrinterState) -> Option<f64> {
    let e_val = raw_e?;
    let delta = if printer.e_absolute {
        e_val - printer.extruder
    } else {
        e_val
    };
    Some(delta)
}

// ─────────────────────────────────────────────────────────────────────────────
// Statistics helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Compute the 3-D Euclidean distance between two points.
#[inline]
fn euclidean_distance(from: &Point3D, to: &Point3D) -> f64 {
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let dz = to.z - from.z;
    (dx * dx + dy * dy + dz * dz).sqrt()
}

/// Update move-count, total distance, and estimated time in `print_stats`.
fn update_move_stats(travel_mm: f64, feedrate: f64, print_stats: &mut PrintStats) {
    print_stats.move_count += 1;
    print_stats.total_distance_mm += travel_mm;
    // Guard against division by zero for a zero feed rate (invalid G-Code, but
    // we prefer a silent no-op over a panic or NaN in the output).
    if feedrate > 0.0 {
        print_stats.estimated_time_seconds += travel_mm / (feedrate / 60.0);
    }
}

/// Expand the axis-aligned bounding box to include the given point.
fn update_bbox(point: &Point3D, print_stats: &mut PrintStats) {
    print_stats.bbox_min.x = print_stats.bbox_min.x.min(point.x);
    print_stats.bbox_min.y = print_stats.bbox_min.y.min(point.y);
    print_stats.bbox_min.z = print_stats.bbox_min.z.min(point.z);
    print_stats.bbox_max.x = print_stats.bbox_max.x.max(point.x);
    print_stats.bbox_max.y = print_stats.bbox_max.y.max(point.y);
    print_stats.bbox_max.z = print_stats.bbox_max.z.max(point.z);
}

// ─────────────────────────────────────────────────────────────────────────────
// Diagnostic emitters
// ─────────────────────────────────────────────────────────────────────────────

/// Emit W001 if any axis of `dest` is negative.
fn emit_negative_coord_warning(line: u32, dest: &Point3D, diags: &mut Vec<Diagnostic>) {
    if dest.x < 0.0 || dest.y < 0.0 || dest.z < 0.0 {
        diags.push(Diagnostic {
            severity: Severity::Warning,
            line,
            code: "W001",
            message: format!(
                "move to negative coordinate ({}, {}, {})",
                dest.x, dest.y, dest.z
            ),
        });
    }
}

/// Emit E001–E003 if `dest` exceeds any machine axis limit.
fn emit_bounds_errors(
    line: u32,
    dest: &Point3D,
    limits: &MachineLimits,
    diags: &mut Vec<Diagnostic>,
) {
    if dest.x > limits.max_x {
        diags.push(Diagnostic {
            severity: Severity::Error,
            line,
            code: "E001",
            message: format!(
                "X {:.3} exceeds machine limit of {:.3} mm",
                dest.x, limits.max_x
            ),
        });
    }
    if dest.y > limits.max_y {
        diags.push(Diagnostic {
            severity: Severity::Error,
            line,
            code: "E002",
            message: format!(
                "Y {:.3} exceeds machine limit of {:.3} mm",
                dest.y, limits.max_y
            ),
        });
    }
    if dest.z > limits.max_z {
        diags.push(Diagnostic {
            severity: Severity::Error,
            line,
            code: "E003",
            message: format!(
                "Z {:.3} exceeds machine limit of {:.3} mm",
                dest.z, limits.max_z
            ),
        });
    }
}

/// Emit W002 (or downgrade to `Info`) for extruder retraction.
///
/// Severity tiers:
/// * `e_delta` in `(-2.0, 0.0)` — [`Severity::Info`] (small / normal retraction)
/// * `e_delta <= -2.0`          — [`Severity::Warning`] (notable retraction)
///
/// Positive deltas (normal extrusion) are silently ignored.
fn emit_retraction_diagnostic(line: u32, e_delta: f64, diags: &mut Vec<Diagnostic>) {
    if e_delta >= 0.0 {
        return;
    }
    let severity = if e_delta > -2.0 {
        Severity::Info
    } else {
        Severity::Warning
    };
    diags.push(Diagnostic {
        severity,
        line,
        code: "W002",
        message: format!("extruder retraction: E delta {e_delta:.3} mm"),
    });
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{GCodeCommand, MachineLimits, Spanned};
    use std::borrow::Cow;

    // ── helpers ──────────────────────────────────────────────────────────────

    /// Wrap a command in a `Spanned` with a given line number.
    fn sp(cmd: GCodeCommand<'static>, line: u32) -> Spanned<GCodeCommand<'static>> {
        Spanned {
            inner: cmd,
            line,
            byte_offset: 0,
        }
    }

    fn linear(
        lx: Option<f64>,
        ly: Option<f64>,
        lz: Option<f64>,
        le: Option<f64>,
        lf: Option<f64>,
    ) -> GCodeCommand<'static> {
        GCodeCommand::LinearMove {
            x: lx,
            y: ly,
            z: lz,
            e: le,
            f: lf,
        }
    }

    fn rapid(
        rx: Option<f64>,
        ry: Option<f64>,
        rz: Option<f64>,
        rf: Option<f64>,
    ) -> GCodeCommand<'static> {
        GCodeCommand::RapidMove {
            x: rx,
            y: ry,
            z: rz,
            f: rf,
        }
    }

    // ── 1. Basic absolute-mode move tracking and bbox ─────────────────────────

    #[test]
    fn test_absolute_move_bbox() {
        let cmds = vec![
            sp(GCodeCommand::SetAbsolute, 1),
            sp(linear(Some(10.0), Some(20.0), Some(5.0), Some(1.0), None), 2),
            sp(linear(Some(50.0), Some(80.0), Some(5.0), Some(2.0), None), 3),
        ];
        let result = analyze(cmds.iter(), None);

        assert_eq!(result.stats.move_count, 2);
        assert!((result.stats.bbox_min.x - 10.0).abs() < f64::EPSILON);
        assert!((result.stats.bbox_min.y - 20.0).abs() < f64::EPSILON);
        assert!((result.stats.bbox_max.x - 50.0).abs() < f64::EPSILON);
        assert!((result.stats.bbox_max.y - 80.0).abs() < f64::EPSILON);
        // No error/warning diagnostics for valid positive-coordinate extrusion.
        let bad: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|d| d.severity >= Severity::Warning)
            .collect();
        assert!(bad.is_empty(), "unexpected diagnostics: {bad:?}");
    }

    // ── 2. G91 relative mode position update ─────────────────────────────────

    #[test]
    fn test_relative_mode_position() {
        let cmds = vec![
            sp(GCodeCommand::SetRelative, 1),
            // From (0,0,0) move +10,+5 → absolute (10, 5, 0).
            sp(linear(Some(10.0), Some(5.0), None, None, None), 2),
            // Then +10,+5 again → (20, 10, 0).
            sp(linear(Some(10.0), Some(5.0), None, None, None), 3),
        ];
        let result = analyze(cmds.iter(), None);

        assert_eq!(result.stats.move_count, 2);
        assert!((result.stats.bbox_max.x - 20.0).abs() < f64::EPSILON);
        assert!((result.stats.bbox_max.y - 10.0).abs() < f64::EPSILON);
    }

    // ── 3. G92 E0 extruder reset ─────────────────────────────────────────────

    #[test]
    fn test_g92_extruder_reset() {
        let cmds = vec![
            sp(GCodeCommand::SetAbsolute, 1),
            // Extrude to E=5.0 — filament delta = 5.0.
            sp(linear(Some(10.0), None, None, Some(5.0), None), 2),
            // G92 E0 — reset extruder logical position to 0.
            sp(
                GCodeCommand::SetPosition {
                    x: None,
                    y: None,
                    z: None,
                    e: Some(0.0),
                },
                3,
            ),
            // Extrude to E=3.0 — delta is 3.0 from the new zero.
            sp(linear(Some(20.0), None, None, Some(3.0), None), 4),
        ];
        let result = analyze(cmds.iter(), None);

        // Total filament should be 5.0 + 3.0 = 8.0, not 5.0 + (3.0−5.0).
        assert!(
            (result.stats.total_filament_mm - 8.0).abs() < 1e-9,
            "expected 8.0 mm filament, got {}",
            result.stats.total_filament_mm
        );
    }

    // ── 4. Out-of-bounds detection ────────────────────────────────────────────

    #[test]
    fn test_out_of_bounds_x() {
        let limits = MachineLimits {
            max_x: 100.0,
            max_y: 100.0,
            max_z: 100.0,
        };
        let cmds = vec![
            sp(GCodeCommand::SetAbsolute, 1),
            sp(linear(Some(150.0), Some(50.0), Some(1.0), None, None), 2),
        ];
        let result = analyze(cmds.iter(), Some(&limits));

        let e001: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|d| d.code == "E001")
            .collect();
        assert_eq!(e001.len(), 1);
        assert_eq!(e001[0].severity, Severity::Error);
        assert_eq!(e001[0].line, 2);
    }

    #[test]
    fn test_out_of_bounds_all_axes() {
        let limits = MachineLimits {
            max_x: 100.0,
            max_y: 100.0,
            max_z: 100.0,
        };
        let cmds = vec![
            sp(GCodeCommand::SetAbsolute, 1),
            sp(rapid(Some(110.0), Some(120.0), Some(130.0), None), 2),
        ];
        let result = analyze(cmds.iter(), Some(&limits));

        let codes: Vec<_> = result.diagnostics.iter().map(|d| d.code).collect();
        assert!(codes.contains(&"E001"), "missing E001 in {codes:?}");
        assert!(codes.contains(&"E002"), "missing E002 in {codes:?}");
        assert!(codes.contains(&"E003"), "missing E003 in {codes:?}");
    }

    #[test]
    fn test_no_bounds_check_without_limits() {
        let cmds = vec![
            sp(GCodeCommand::SetAbsolute, 1),
            sp(
                linear(Some(9999.0), Some(9999.0), Some(9999.0), None, None),
                2,
            ),
        ];
        // No limits supplied — should produce zero Error diagnostics.
        let result = analyze(cmds.iter(), None);
        let errors: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        assert!(errors.is_empty());
    }

    // ── 5. OrcaSlicer ;LAYER_CHANGE comment increments layer count ────────────

    #[test]
    fn test_orca_layer_change_comment() {
        let cmds = vec![
            sp(
                GCodeCommand::Comment {
                    text: Cow::Borrowed("LAYER_CHANGE"),
                },
                1,
            ),
            sp(
                GCodeCommand::Comment {
                    text: Cow::Borrowed("LAYER_CHANGE"),
                },
                2,
            ),
            sp(
                GCodeCommand::Comment {
                    text: Cow::Borrowed("some other comment"),
                },
                3,
            ),
        ];
        let result = analyze(cmds.iter(), None);

        assert_eq!(result.stats.layer_count, 2);
    }

    // ── 6. Z-based layer detection fallback ──────────────────────────────────

    #[test]
    fn test_z_based_layer_detection() {
        let cmds = vec![
            sp(GCodeCommand::SetAbsolute, 1),
            // Z=0.2 — first layer.
            sp(linear(Some(10.0), None, Some(0.2), Some(1.0), None), 2),
            // Z=0.4 — second layer.
            sp(linear(Some(20.0), None, Some(0.4), Some(1.0), None), 3),
            // Z stays at 0.4 — no new layer.
            sp(linear(Some(30.0), None, Some(0.4), Some(1.0), None), 4),
        ];
        let result = analyze(cmds.iter(), None);

        // Two Z increases: 0→0.2 and 0.2→0.4.
        assert_eq!(result.stats.layer_count, 2);

        let i001: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|d| d.code == "I001")
            .collect();
        assert_eq!(i001.len(), 2);
        assert_eq!(i001[0].severity, Severity::Info);
    }

    // ── 7. Estimated time with non-default feedrate ───────────────────────────

    #[test]
    fn test_estimated_time_with_feedrate() {
        // 100 mm move at 6 000 mm/min = 1.0 second.
        let cmds = vec![
            sp(GCodeCommand::SetAbsolute, 1),
            sp(linear(Some(100.0), None, None, None, Some(6_000.0)), 2),
        ];
        let result = analyze(cmds.iter(), None);

        // distance = 100 mm, feedrate = 6000 mm/min → time = 100 / (6000/60) = 1.0 s
        assert!(
            (result.stats.estimated_time_seconds - 1.0).abs() < 1e-9,
            "expected 1.0 s, got {}",
            result.stats.estimated_time_seconds
        );
    }

    // ── 8. Retraction diagnostic tiers ───────────────────────────────────────

    #[test]
    fn test_retraction_info_small() {
        // Small retraction: absolute E 5.0 → 4.5, delta = -0.5 (< 2.0 mm).
        let cmds = vec![
            sp(GCodeCommand::SetAbsolute, 1),
            sp(linear(None, None, None, Some(5.0), None), 2),
            sp(linear(None, None, None, Some(4.5), None), 3),
        ];
        let result = analyze(cmds.iter(), None);

        let w002: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|d| d.code == "W002")
            .collect();
        assert_eq!(w002.len(), 1);
        assert_eq!(w002[0].severity, Severity::Info);
    }

    #[test]
    fn test_retraction_warning_large() {
        // Large retraction: absolute E 10.0 → 5.0, delta = -5.0.
        let cmds = vec![
            sp(GCodeCommand::SetAbsolute, 1),
            sp(linear(None, None, None, Some(10.0), None), 2),
            sp(linear(None, None, None, Some(5.0), None), 3),
        ];
        let result = analyze(cmds.iter(), None);

        let w002: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|d| d.code == "W002")
            .collect();
        assert_eq!(w002.len(), 1);
        assert_eq!(w002[0].severity, Severity::Warning);
    }

    // ── 9. Negative coordinate warning ───────────────────────────────────────

    #[test]
    fn test_negative_coordinate_warning() {
        let cmds = vec![
            sp(GCodeCommand::SetAbsolute, 1),
            sp(linear(Some(-1.0), Some(10.0), Some(0.0), None, None), 2),
        ];
        let result = analyze(cmds.iter(), None);

        let w001: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|d| d.code == "W001")
            .collect();
        assert_eq!(w001.len(), 1);
        assert_eq!(w001[0].severity, Severity::Warning);
    }

    // ── 10. Filament accounting: only positive deltas count ──────────────────

    #[test]
    fn test_filament_only_positive_deltas() {
        let cmds = vec![
            sp(GCodeCommand::SetAbsolute, 1),
            sp(linear(Some(10.0), None, None, Some(5.0), None), 2),
            // Retract to 3.0 — delta = -2.0, should NOT add to filament_mm.
            sp(linear(Some(20.0), None, None, Some(3.0), None), 3),
            // Extrude again to 8.0 — delta = +5.0.
            sp(linear(Some(30.0), None, None, Some(8.0), None), 4),
        ];
        let result = analyze(cmds.iter(), None);

        // Positive deltas only: 5.0 + 5.0 = 10.0 mm.
        assert!(
            (result.stats.total_filament_mm - 10.0).abs() < 1e-9,
            "expected 10.0, got {}",
            result.stats.total_filament_mm
        );
    }
}
