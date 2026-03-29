//! Conservative G-Code optimizer.
//!
//! This module implements a single optimization pass over a parsed G-Code
//! command list.  It is deliberately conservative: only commands that are
//! *mathematically certain* to have no observable effect are removed.  No
//! commands are reordered.
//!
//! # Supported rules
//!
//! | Rule | Description |
//! |------|-------------|
//! | 1 | Empty move — G0/G1 with **all** of x/y/z/e/f = `None` |
//! | 2 | Duplicate consecutive mode switch — G90 after G90, G91 after G91 |
//! | 3 | Duplicate consecutive fan command — M106/M107 with same params |
//! | 4 | Zero-delta move — absolute move to current position with no feedrate change |
//! | 5 | Duplicate consecutive temperature command — M104/M109/M140/M190 |
//! | 7 | Consecutive same-axis travel — first non-extruding single-axis move superseded by next |
//!
//! # Dry-run mode
//!
//! When [`OptConfig::dry_run`] is `true` the returned [`OptimizationResult`]
//! reports all changes that *would* be made but leaves the command list
//! untouched.

#![warn(clippy::pedantic)]

use crate::diagnostics::OptimizationChange;
use crate::models::{GCodeCommand, Spanned};

// ─────────────────────────────────────────────────────────────────────────────
// Public types
// ─────────────────────────────────────────────────────────────────────────────

/// Configuration for the optimizer.
#[derive(Debug, Clone, Default)]
pub struct OptConfig {
    /// When `true`, compute all changes but return the original command list
    /// unchanged.  Callers can inspect [`OptimizationResult::changes`] to
    /// preview what would be modified.
    pub dry_run: bool,

    /// When `true`, merge collinear consecutive G1 moves into single moves.
    ///
    /// Detects three or more consecutive G1 commands on the same 3D line
    /// with consistent feedrate and proportional extrusion, replacing them
    /// with a single move.  Opt-in because it modifies move structure.
    pub merge_collinear: bool,

    /// When `true`, strip existing M73 progress markers and re-insert
    /// recalculated ones at each layer boundary.
    pub insert_progress: bool,
}

/// The result of a single optimization pass.
#[derive(Debug)]
pub struct OptimizationResult<'a> {
    /// The (possibly modified) command list.
    ///
    /// In dry-run mode this is identical to the input slice — no commands are
    /// removed or altered.
    pub commands: Vec<Spanned<GCodeCommand<'a>>>,

    /// All changes made (or that would be made in dry-run mode), one entry per
    /// redundant command detected.
    pub changes: Vec<OptimizationChange>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Entry point
// ─────────────────────────────────────────────────────────────────────────────

/// Run the optimizer over a list of commands.
///
/// Takes ownership of `commands`.  Returns an [`OptimizationResult`] whose
/// `.commands` field is the filtered list (or the original list in dry-run
/// mode) and whose `.changes` field lists every redundant command found.
///
/// The pass is deterministic: identical input always produces identical output.
#[must_use]
pub fn optimize<'a>(
    commands: Vec<Spanned<GCodeCommand<'a>>>,
    config: &OptConfig,
) -> OptimizationResult<'a> {
    let mut changes: Vec<OptimizationChange> = Vec::new();

    // Bit-vector: true = this command is redundant and should be removed.
    let mut redundant: Vec<bool> = vec![false; commands.len()];

    // ── Pass: walk commands once, maintaining state ───────────────────────────
    let mut state = PassState::new();

    for (idx, spanned) in commands.iter().enumerate() {
        let cmd = &spanned.inner;

        // Check each rule in priority order.  We stop after the first match so
        // a single command is only flagged once.
        let description = check_rules(cmd, &state);

        if let Some(desc) = description {
            redundant[idx] = true;
            changes.push(OptimizationChange {
                line: spanned.line,
                description: desc,
            });
        }

        // ── Rule 7: consecutive same-axis travel ─────────────────────────────
        // When a new single-axis non-extruding move supersedes a prior one on
        // the same axis with the same feedrate, the prior move is redundant —
        // the printer will skip straight to the final position anyway.
        // Comments and Unknown commands are transparent (do not break the chain).
        if let Some((current_axis, current_feed)) = single_axis_travel(cmd) {
            if let Some((prev_idx, ref prev_travel)) = state.last_single_axis_travel {
                if prev_travel.axis == current_axis
                    && prev_travel.feedrate == current_feed
                    && !redundant[prev_idx]
                {
                    redundant[prev_idx] = true;
                    changes.push(OptimizationChange {
                        line: commands[prev_idx].line,
                        description: "redundant same-axis travel (superseded by next move)"
                            .to_owned(),
                    });
                }
            }
            state.last_single_axis_travel =
                Some((idx, SingleAxisTravel { axis: current_axis, feedrate: current_feed }));
        } else {
            match cmd {
                // Comments and unknown tokens are transparent — preserve the
                // pending travel so it can still be matched by a later move.
                GCodeCommand::Comment { .. } | GCodeCommand::Unknown { .. } => {}
                // Any other real command breaks the detection chain.
                _ => {
                    state.last_single_axis_travel = None;
                }
            }
        }

        // Always advance state — even redundant commands affect mode tracking
        // (e.g. a duplicate G90 still leaves us in absolute mode).
        state.update(cmd);
    }

    // ── Assemble result ───────────────────────────────────────────────────────
    let output_commands = if config.dry_run {
        // Dry-run: return the original list untouched.
        commands
    } else {
        // Filter out redundant commands while preserving order.
        commands
            .into_iter()
            .zip(redundant.iter())
            .filter_map(|(cmd, &is_redundant)| if is_redundant { None } else { Some(cmd) })
            .collect()
    };

    // Rule 7 can push change entries out of source order (the redundant
    // command is the *earlier* one, but it is discovered while processing the
    // *later* command).  Sort so callers always see changes in line order.
    changes.sort_by_key(|c| c.line);

    OptimizationResult {
        commands: output_commands,
        changes,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Move parameter bundle
// ─────────────────────────────────────────────────────────────────────────────

/// All axis and feedrate parameters for a single G0/G1 move, bundled into a
/// named struct to avoid triggering `clippy::many_single_char_names` when the
/// five fields appear as separate function parameters.
#[derive(Debug, Clone, Copy)]
struct MoveAxes {
    /// Target X coordinate, if specified.
    x_pos: Option<f64>,
    /// Target Y coordinate, if specified.
    y_pos: Option<f64>,
    /// Target Z coordinate, if specified.
    z_pos: Option<f64>,
    /// Target E (extruder) coordinate, if specified.
    extrude: Option<f64>,
    /// Target feedrate, if specified.
    feed: Option<f64>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Rule checker
// ─────────────────────────────────────────────────────────────────────────────

/// Returns `Some(description)` if `cmd` is redundant given `state`, or `None`
/// if the command should be kept.
fn check_rules(cmd: &GCodeCommand<'_>, state: &PassState) -> Option<String> {
    match cmd {
        // ── Rule 1: empty move ────────────────────────────────────────────────
        GCodeCommand::RapidMove { x: None, y: None, z: None, f: None } |
        GCodeCommand::LinearMove { x: None, y: None, z: None, e: None, f: None } => {
            Some("empty move with no parameters".to_owned())
        }

        // ── Rule 4 + feedrate-only guard: moves with at least one field set ───
        GCodeCommand::RapidMove { x, y, z, f } => {
            check_zero_delta_move(MoveAxes { x_pos: *x, y_pos: *y, z_pos: *z, extrude: None, feed: *f }, state)
        }
        GCodeCommand::LinearMove { x, y, z, e, f } => {
            check_zero_delta_move(MoveAxes { x_pos: *x, y_pos: *y, z_pos: *z, extrude: *e, feed: *f }, state)
        }

        // ── Rule 2: duplicate consecutive mode switch ─────────────────────────
        GCodeCommand::SetAbsolute => {
            if state.last_mode == Some(PositioningMode::Absolute) {
                Some("duplicate mode switch (G90/G91 already active)".to_owned())
            } else {
                None
            }
        }
        GCodeCommand::SetRelative => {
            if state.last_mode == Some(PositioningMode::Relative) {
                Some("duplicate mode switch (G90/G91 already active)".to_owned())
            } else {
                None
            }
        }

        // ── Rule 3: duplicate consecutive fan command ─────────────────────────
        GCodeCommand::MetaCommand { code: 106, params } => {
            if state.last_m106_params.as_deref() == Some(params.as_ref()) {
                Some("duplicate fan command (same setting already active)".to_owned())
            } else {
                None
            }
        }
        GCodeCommand::MetaCommand { code: 107, params } => {
            if state.last_m107_params.as_deref() == Some(params.as_ref()) {
                Some("duplicate fan command (same setting already active)".to_owned())
            } else {
                None
            }
        }

        // ── Rule 5: duplicate consecutive temperature command ─────────────────
        GCodeCommand::MetaCommand { code: code @ (104 | 109), params } => {
            let last = if *code == 104 {
                state.last_m104_params.as_deref()
            } else {
                state.last_m109_params.as_deref()
            };
            if last == Some(params.as_ref()) {
                Some("duplicate temperature command (already set)".to_owned())
            } else {
                None
            }
        }
        GCodeCommand::MetaCommand { code: code @ (140 | 190), params } => {
            let last = if *code == 140 {
                state.last_m140_params.as_deref()
            } else {
                state.last_m190_params.as_deref()
            };
            if last == Some(params.as_ref()) {
                Some("duplicate temperature command (already set)".to_owned())
            } else {
                None
            }
        }

        // Everything else is kept.
        _ => None,
    }
}

/// Rule 4 helper: returns `Some(description)` if this is a zero-delta absolute
/// move with no feedrate change, otherwise `None`.
///
/// The feedrate-only exception: if only `feed` is `Some` and all axis fields
/// are `None` — that is a valid feedrate-set command and must not be removed.
/// (Note: if all fields are `None`, Rule 1 catches it first and this function
/// is never reached for that case.)
fn check_zero_delta_move(axes: MoveAxes, state: &PassState) -> Option<String> {
    // Only applies in absolute mode.
    if state.pos.is_absolute {
        // If a feedrate change is requested, keep the command regardless.
        if axes.feed.is_some() {
            return None;
        }

        // All specified axis values must equal the current tracked position.
        let x_ok = axes.x_pos.map_or(true, |v| (v - state.pos.x).abs() < POSITION_TOLERANCE);
        let y_ok = axes.y_pos.map_or(true, |v| (v - state.pos.y).abs() < POSITION_TOLERANCE);
        let z_ok = axes.z_pos.map_or(true, |v| (v - state.pos.z).abs() < POSITION_TOLERANCE);
        let e_ok = axes.extrude.map_or(true, |v| (v - state.pos.e).abs() < POSITION_TOLERANCE);

        if x_ok && y_ok && z_ok && e_ok {
            // The case where all of x_pos/y_pos/z_pos/extrude are None (and
            // feed is None too) is caught by Rule 1 before we get here, so
            // reaching this branch implies at least one axis was specified.
            return Some("zero-delta move (no net displacement)".to_owned());
        }
    }

    None
}

// ─────────────────────────────────────────────────────────────────────────────
// Position / mode tracking
// ─────────────────────────────────────────────────────────────────────────────

/// Tolerance for floating-point position comparisons (0.1 µm).
const POSITION_TOLERANCE: f64 = 0.000_1;

/// Absolute vs relative positioning mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PositioningMode {
    Absolute,
    Relative,
}

/// Simulated machine position used for Rule 4 evaluation.
#[derive(Debug, Clone)]
struct PositionState {
    /// Current X position (mm), in absolute coordinates.
    x: f64,
    /// Current Y position (mm), in absolute coordinates.
    y: f64,
    /// Current Z position (mm), in absolute coordinates.
    z: f64,
    /// Current E (extruder) position (mm), in absolute coordinates.
    e: f64,
    /// Whether positioning is currently absolute (`G90`) or relative (`G91`).
    is_absolute: bool,
    /// Most recent feedrate seen (mm/min).  Not used for redundancy checks
    /// but maintained for completeness.
    #[allow(dead_code)]
    feedrate: f64,
}

impl Default for PositionState {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            e: 0.0,
            // 3D printers almost universally start in absolute mode.
            is_absolute: true,
            feedrate: 0.0,
        }
    }
}

/// Which single axis a non-extruding travel move affected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SingleAxis {
    X,
    Y,
    Z,
}

/// State for Rule 7: last single-axis non-extruding travel.
#[derive(Debug, Clone)]
struct SingleAxisTravel {
    axis: SingleAxis,
    feedrate: Option<f64>,
}

/// Returns the single axis affected by a non-extruding move, or `None`.
///
/// A qualifying move must set exactly one of X/Y/Z, must not set E (no
/// extrusion), and must be a `RapidMove` or `LinearMove`.
fn single_axis_travel(cmd: &GCodeCommand<'_>) -> Option<(SingleAxis, Option<f64>)> {
    let params = match cmd {
        GCodeCommand::RapidMove { x, y, z, f } => {
            MoveAxes { x_pos: *x, y_pos: *y, z_pos: *z, extrude: None, feed: *f }
        }
        GCodeCommand::LinearMove { x, y, z, e, f } => {
            MoveAxes { x_pos: *x, y_pos: *y, z_pos: *z, extrude: *e, feed: *f }
        }
        _ => return None,
    };
    if params.extrude.is_some() {
        return None;
    }
    let x_set = params.x_pos.is_some();
    let y_set = params.y_pos.is_some();
    let z_set = params.z_pos.is_some();
    let count = u8::from(x_set) + u8::from(y_set) + u8::from(z_set);
    if count != 1 {
        return None;
    }
    let affected = if x_set {
        SingleAxis::X
    } else if y_set {
        SingleAxis::Y
    } else {
        SingleAxis::Z
    };
    Some((affected, params.feed))
}

/// All mutable state maintained during a single optimizer pass.
#[derive(Debug)]
struct PassState {
    /// Current machine position and mode.
    pos: PositionState,

    /// The last positioning mode set by G90/G91, `None` before any such
    /// command is seen.  `Comment` and `Unknown` commands do not update this
    /// field, so consecutive G90s separated only by comments are still caught.
    last_mode: Option<PositioningMode>,

    /// Parameter string of the last M106 seen (fan on), `None` before any M106.
    last_m106_params: Option<String>,
    /// Parameter string of the last M107 seen (fan off), `None` before any M107.
    last_m107_params: Option<String>,

    /// Parameter string of the last M104 seen (hotend temp, no wait).
    last_m104_params: Option<String>,
    /// Parameter string of the last M109 seen (hotend temp, wait).
    last_m109_params: Option<String>,
    /// Parameter string of the last M140 seen (bed temp, no wait).
    last_m140_params: Option<String>,
    /// Parameter string of the last M190 seen (bed temp, wait).
    last_m190_params: Option<String>,

    /// Index and details of the last single-axis non-extruding travel, for
    /// Rule 7.  Carries forward through `Comment` and `Unknown` commands so
    /// that purely-comment-separated moves are still collapsed.
    last_single_axis_travel: Option<(usize, SingleAxisTravel)>,
}

impl PassState {
    fn new() -> Self {
        Self {
            pos: PositionState::default(),
            last_mode: None,
            last_m106_params: None,
            last_m107_params: None,
            last_m104_params: None,
            last_m109_params: None,
            last_m140_params: None,
            last_m190_params: None,
            last_single_axis_travel: None,
        }
    }

    /// Advance state based on a command.
    ///
    /// This must be called for **every** command (including redundant ones) so
    /// that mode and position tracking remains correct.
    fn update(&mut self, cmd: &GCodeCommand<'_>) {
        match cmd {
            GCodeCommand::SetAbsolute => {
                self.pos.is_absolute = true;
                self.last_mode = Some(PositioningMode::Absolute);
            }
            GCodeCommand::SetRelative => {
                self.pos.is_absolute = false;
                self.last_mode = Some(PositioningMode::Relative);
            }

            GCodeCommand::SetPosition { x, y, z, e } => {
                // G92 overrides the logical position without physical movement.
                if let Some(v) = x {
                    self.pos.x = *v;
                }
                if let Some(v) = y {
                    self.pos.y = *v;
                }
                if let Some(v) = z {
                    self.pos.z = *v;
                }
                if let Some(v) = e {
                    self.pos.e = *v;
                }
                // A bare G92 (no params) resets all axes to 0.
                if x.is_none() && y.is_none() && z.is_none() && e.is_none() {
                    self.pos.x = 0.0;
                    self.pos.y = 0.0;
                    self.pos.z = 0.0;
                    self.pos.e = 0.0;
                }
            }

            GCodeCommand::RapidMove { x, y, z, f } => {
                self.apply_move(MoveAxes { x_pos: *x, y_pos: *y, z_pos: *z, extrude: None, feed: *f });
            }
            GCodeCommand::LinearMove { x, y, z, e, f } => {
                self.apply_move(MoveAxes { x_pos: *x, y_pos: *y, z_pos: *z, extrude: *e, feed: *f });
            }

            GCodeCommand::MetaCommand { code: 106, params } => {
                self.last_m106_params = Some(params.as_ref().to_owned());
            }
            GCodeCommand::MetaCommand { code: 107, params } => {
                self.last_m107_params = Some(params.as_ref().to_owned());
            }
            GCodeCommand::MetaCommand { code: 104, params } => {
                self.last_m104_params = Some(params.as_ref().to_owned());
            }
            GCodeCommand::MetaCommand { code: 109, params } => {
                self.last_m109_params = Some(params.as_ref().to_owned());
            }
            GCodeCommand::MetaCommand { code: 140, params } => {
                self.last_m140_params = Some(params.as_ref().to_owned());
            }
            GCodeCommand::MetaCommand { code: 190, params } => {
                self.last_m190_params = Some(params.as_ref().to_owned());
            }

            // Comments, Unknown, GCommand, and all other MetaCommands do not
            // affect mode or position state.
            _ => {}
        }
    }

    /// Apply a G0/G1 move to the tracked position.
    fn apply_move(&mut self, axes: MoveAxes) {
        if self.pos.is_absolute {
            if let Some(v) = axes.x_pos {
                self.pos.x = v;
            }
            if let Some(v) = axes.y_pos {
                self.pos.y = v;
            }
            if let Some(v) = axes.z_pos {
                self.pos.z = v;
            }
            if let Some(v) = axes.extrude {
                self.pos.e = v;
            }
        } else {
            // Relative mode: add the delta.
            self.pos.x += axes.x_pos.unwrap_or(0.0);
            self.pos.y += axes.y_pos.unwrap_or(0.0);
            self.pos.z += axes.z_pos.unwrap_or(0.0);
            self.pos.e += axes.extrude.unwrap_or(0.0);
        }
        if let Some(v) = axes.feed {
            self.pos.feedrate = v;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;
    use crate::models::GCodeCommand;

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Wrap a command at a synthetic line number for test use.
    fn spanned(cmd: GCodeCommand<'static>, line: u32) -> Spanned<GCodeCommand<'static>> {
        Spanned { inner: cmd, line, byte_offset: 0 }
    }

    fn g1_empty() -> GCodeCommand<'static> {
        GCodeCommand::LinearMove { x: None, y: None, z: None, e: None, f: None }
    }

    fn g1_at(x: f64, y: f64) -> GCodeCommand<'static> {
        GCodeCommand::LinearMove { x: Some(x), y: Some(y), z: None, e: None, f: None }
    }

    fn g1_f_only(f: f64) -> GCodeCommand<'static> {
        GCodeCommand::LinearMove { x: None, y: None, z: None, e: None, f: Some(f) }
    }

    fn g90() -> GCodeCommand<'static> {
        GCodeCommand::SetAbsolute
    }

    fn m106(params: &'static str) -> GCodeCommand<'static> {
        GCodeCommand::MetaCommand { code: 106, params: Cow::Borrowed(params) }
    }

    fn m104(params: &'static str) -> GCodeCommand<'static> {
        GCodeCommand::MetaCommand { code: 104, params: Cow::Borrowed(params) }
    }

    fn m1_misc(code: u16) -> GCodeCommand<'static> {
        GCodeCommand::MetaCommand { code, params: Cow::Borrowed("") }
    }

    // ── Rule 1: empty G1 ─────────────────────────────────────────────────────

    #[test]
    fn empty_g1_is_removed() {
        let cmds = vec![spanned(g1_empty(), 1)];
        let result = optimize(cmds, &OptConfig::default());
        assert!(result.commands.is_empty(), "empty G1 should be removed");
        assert_eq!(result.changes.len(), 1);
        assert_eq!(result.changes[0].description, "empty move with no parameters");
    }

    #[test]
    fn non_empty_g1_is_preserved() {
        let cmds = vec![spanned(g1_at(10.0, 20.0), 1)];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(result.commands.len(), 1, "non-empty G1 must be kept");
        assert!(result.changes.is_empty());
    }

    // ── Rule 2: duplicate G90 ────────────────────────────────────────────────

    #[test]
    fn duplicate_g90_second_removed() {
        let cmds = vec![
            spanned(g90(), 1),
            spanned(g90(), 2),
        ];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(result.commands.len(), 1, "second G90 must be removed");
        assert_eq!(result.commands[0].line, 1);
        assert_eq!(result.changes.len(), 1);
        assert_eq!(
            result.changes[0].description,
            "duplicate mode switch (G90/G91 already active)"
        );
    }

    #[test]
    fn duplicate_g90_with_comment_between_still_removed() {
        // Comments are transparent for duplicate-detection purposes.
        let cmds = vec![
            spanned(g90(), 1),
            spanned(GCodeCommand::Comment { text: Cow::Borrowed("a comment") }, 2),
            spanned(g90(), 3),
        ];
        let result = optimize(cmds, &OptConfig::default());
        // The comment is kept; only the second G90 is removed.
        assert_eq!(result.commands.len(), 2);
        assert_eq!(result.changes.len(), 1);
        assert_eq!(result.changes[0].line, 3);
    }

    // ── Rule 3: duplicate M106 ───────────────────────────────────────────────

    #[test]
    fn duplicate_m106_same_params_removed() {
        let cmds = vec![
            spanned(m106("S255"), 1),
            spanned(m106("S255"), 2),
        ];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(result.commands.len(), 1);
        assert_eq!(
            result.changes[0].description,
            "duplicate fan command (same setting already active)"
        );
    }

    #[test]
    fn m106_different_params_both_kept() {
        let cmds = vec![
            spanned(m106("S128"), 1),
            spanned(m106("S255"), 2),
        ];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(result.commands.len(), 2);
        assert!(result.changes.is_empty());
    }

    // ── Rule 4: zero-delta move ──────────────────────────────────────────────

    #[test]
    fn zero_delta_move_in_absolute_mode_removed() {
        // Start at (0, 0), then issue G1 X0 Y0 — no displacement.
        let cmds = vec![
            spanned(g90(), 1),           // ensure absolute mode
            spanned(g1_at(0.0, 0.0), 2), // zero-delta from default origin
        ];
        let result = optimize(cmds, &OptConfig::default());
        // G90 kept, G1 removed.
        assert_eq!(result.commands.len(), 1);
        assert_eq!(result.changes.len(), 1);
        assert_eq!(
            result.changes[0].description,
            "zero-delta move (no net displacement)"
        );
    }

    #[test]
    fn zero_delta_after_real_move_removed() {
        // Move to (10, 20), then issue another G1 X10 Y20.
        let cmds = vec![
            spanned(g90(), 1),
            spanned(g1_at(10.0, 20.0), 2), // actual move
            spanned(g1_at(10.0, 20.0), 3), // zero-delta — should be removed
        ];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(result.commands.len(), 2, "second move to same pos should be removed");
        assert_eq!(result.changes.len(), 1);
        assert_eq!(result.changes[0].line, 3);
    }

    #[test]
    fn feedrate_only_g1_not_removed() {
        // G1 F3000 — feedrate-only command; Rule 4 exception.
        let cmds = vec![
            spanned(g90(), 1),
            spanned(g1_f_only(3000.0), 2),
        ];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(result.commands.len(), 2, "feedrate-only G1 must be preserved");
        assert!(result.changes.is_empty());
    }

    #[test]
    fn zero_delta_with_feedrate_not_removed() {
        // G1 X0 Y0 F3000 — moves to current position but changes feedrate.
        // The feedrate change is meaningful, so keep it.
        let cmds = vec![
            spanned(g90(), 1),
            spanned(
                GCodeCommand::LinearMove { x: Some(0.0), y: Some(0.0), z: None, e: None, f: Some(3000.0) },
                2,
            ),
        ];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(result.commands.len(), 2, "move with feedrate change must be kept");
        assert!(result.changes.is_empty());
    }

    #[test]
    fn zero_delta_in_relative_mode_not_removed() {
        // In relative mode, G1 X0 Y0 is explicit user intent (e.g. to set feedrate
        // implicitly or as a deliberate no-op delay).
        let cmds = vec![
            spanned(GCodeCommand::SetRelative, 1),
            spanned(g1_at(0.0, 0.0), 2),
        ];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(result.commands.len(), 2, "zero move in relative mode must be kept");
        assert!(result.changes.is_empty());
    }

    fn g0_x(x: f64) -> GCodeCommand<'static> {
        GCodeCommand::RapidMove { x: Some(x), y: None, z: None, f: None }
    }

    fn g0_x_f(x: f64, f: f64) -> GCodeCommand<'static> {
        GCodeCommand::RapidMove { x: Some(x), y: None, z: None, f: Some(f) }
    }

    fn g0_y(y: f64) -> GCodeCommand<'static> {
        GCodeCommand::RapidMove { x: None, y: Some(y), z: None, f: None }
    }

    fn g1_x_e(x: f64, e: f64) -> GCodeCommand<'static> {
        GCodeCommand::LinearMove { x: Some(x), y: None, z: None, e: Some(e), f: None }
    }

    // ── Rule 7: consecutive same-axis travel merging ─────────────────────────

    #[test]
    fn test_rule7_same_axis_rapid_first_removed() {
        let cmds = vec![
            spanned(g0_x(10.0), 1),
            spanned(g0_x(20.0), 2),
        ];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(result.commands.len(), 1, "first G0 X should be removed");
        assert_eq!(result.commands[0].line, 2);
        assert_eq!(result.changes.len(), 1);
    }

    #[test]
    fn test_rule7_different_axes_both_kept() {
        let cmds = vec![
            spanned(g0_x(10.0), 1),
            spanned(g0_y(20.0), 2),
        ];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(result.commands.len(), 2, "different axes must both be kept");
        assert!(result.changes.is_empty());
    }

    #[test]
    fn test_rule7_extruding_moves_both_kept() {
        let cmds = vec![
            spanned(g1_x_e(10.0, 1.0), 1),
            spanned(g1_x_e(20.0, 2.0), 2),
        ];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(result.commands.len(), 2, "extruding moves must both be kept");
        assert!(result.changes.is_empty());
    }

    #[test]
    fn test_rule7_different_feedrates_both_kept() {
        let cmds = vec![
            spanned(g0_x_f(10.0, 3000.0), 1),
            spanned(g0_x_f(20.0, 6000.0), 2),
        ];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(result.commands.len(), 2, "different feedrates must both be kept");
        assert!(result.changes.is_empty());
    }

    #[test]
    fn test_rule7_comment_between_still_removed() {
        let cmds = vec![
            spanned(g0_x(10.0), 1),
            spanned(GCodeCommand::Comment { text: Cow::Borrowed("move") }, 2),
            spanned(g0_x(20.0), 3),
        ];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(result.commands.len(), 2, "first G0 X should be removed through comment");
        let kept_lines: Vec<u32> = result.commands.iter().map(|s| s.line).collect();
        assert_eq!(kept_lines, vec![2, 3]);
    }

    #[test]
    fn test_rule7_multi_axis_move_not_matched() {
        let cmds = vec![
            spanned(GCodeCommand::RapidMove { x: Some(10.0), y: Some(20.0), z: None, f: None }, 1),
            spanned(GCodeCommand::RapidMove { x: Some(30.0), y: Some(40.0), z: None, f: None }, 2),
        ];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(result.commands.len(), 2, "multi-axis moves must both be kept");
    }

    // ── Rule 5: duplicate temperature command ─────────────────────────────────

    #[test]
    fn duplicate_m104_same_params_removed() {
        let cmds = vec![
            spanned(m104("S200"), 1),
            spanned(m104("S200"), 2),
        ];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(result.commands.len(), 1);
        assert_eq!(
            result.changes[0].description,
            "duplicate temperature command (already set)"
        );
    }

    #[test]
    fn m104_different_temps_both_kept() {
        let cmds = vec![
            spanned(m104("S200"), 1),
            spanned(m104("S210"), 2),
        ];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(result.commands.len(), 2);
        assert!(result.changes.is_empty());
    }

    // ── Dry-run mode ─────────────────────────────────────────────────────────

    #[test]
    fn dry_run_reports_changes_but_does_not_modify_commands() {
        let cmds = vec![
            spanned(g1_empty(), 1),
            spanned(g1_at(5.0, 5.0), 2),
        ];
        let config = OptConfig { dry_run: true, ..Default::default() };
        let result = optimize(cmds, &config);

        // Command list is unchanged.
        assert_eq!(result.commands.len(), 2, "dry-run must not remove any commands");
        // But changes are still reported.
        assert_eq!(result.changes.len(), 1);
        assert_eq!(result.changes[0].line, 1);
    }

    // ── Mixed: 10 commands, 3 redundant → 7 in output ────────────────────────

    #[test]
    fn mixed_file_three_redundant_seven_kept() {
        let cmds = vec![
            spanned(g90(), 1),                              // kept: first G90
            spanned(g90(), 2),                              // REMOVED: duplicate G90
            spanned(g1_at(10.0, 10.0), 3),                 // kept: actual move
            spanned(g1_at(10.0, 10.0), 4),                 // REMOVED: zero-delta
            spanned(m104("S200"), 5),                       // kept: first M104
            spanned(m104("S200"), 6),                       // REMOVED: duplicate M104
            spanned(m106("S255"), 7),                       // kept: first M106
            spanned(g1_at(20.0, 30.0), 8),                 // kept: actual move
            spanned(m1_misc(84), 9),                        // kept: M84 (motors off)
            spanned(g1_f_only(3000.0), 10),                 // kept: feedrate-only
        ];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(result.commands.len(), 7, "expected 7 commands after removing 3 redundant");
        assert_eq!(result.changes.len(), 3, "expected exactly 3 reported changes");

        // Verify the surviving line numbers.
        let kept_lines: Vec<u32> = result.commands.iter().map(|s| s.line).collect();
        assert_eq!(kept_lines, vec![1, 3, 5, 7, 8, 9, 10]);
    }
}
