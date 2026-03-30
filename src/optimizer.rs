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

use std::borrow::Cow;

use crate::diagnostics::{Diagnostic, OptimizationChange, Severity};
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
#[allow(clippy::too_many_lines)] // Rules checked inline; extracting sub-fns would scatter the logic.
pub fn optimize<'a>(
    commands: Vec<Spanned<GCodeCommand<'a>>>,
    config: &OptConfig,
) -> OptimizationResult<'a> {
    let mut changes: Vec<OptimizationChange> = Vec::new();

    // Bit-vector: true = this command is redundant and should be removed.
    let mut redundant: Vec<bool> = vec![false; commands.len()];

    // Modification table: `Some(cmd)` = replace this slot's inner command.
    // Indexed in parallel with `commands`.  Rule 8 writes here instead of
    // marking a command redundant — the move survives but loses its `f` field.
    let mut modifications: Vec<Option<GCodeCommand<'a>>> = vec![None; commands.len()];

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

        // ── Rule 8: redundant feedrate elimination ───────────────────────────
        // Strip the `f` field from moves whose feedrate matches the current
        // modal feedrate.  The command is not removed — it is transformed.
        // Only applies to non-redundant commands (no point modifying a move
        // that will be discarded anyway).
        //
        // Rule 8 runs BEFORE Rule 7 so that the effective (post-strip) feedrate
        // is used when Rule 7 checks for consecutive same-axis travel.  This
        // prevents a move that changes axis position (but not feedrate) from
        // being incorrectly collapsed by Rule 7 just because it originally
        // carried a now-redundant F parameter.
        if !redundant[idx] {
            let cmd_feed = match cmd {
                GCodeCommand::RapidMove { f, .. } | GCodeCommand::LinearMove { f, .. } => *f,
                _ => None,
            };
            if let Some(f_val) = cmd_feed {
                if let Some(modal) = state.modal_feedrate {
                    if (f_val - modal).abs() < FEEDRATE_TOLERANCE {
                        let stripped = match cmd {
                            GCodeCommand::RapidMove { x, y, z, .. } => GCodeCommand::RapidMove {
                                x: *x,
                                y: *y,
                                z: *z,
                                f: None,
                            },
                            GCodeCommand::LinearMove { x, y, z, e, .. } => {
                                GCodeCommand::LinearMove {
                                    x: *x,
                                    y: *y,
                                    z: *z,
                                    e: *e,
                                    f: None,
                                }
                            }
                            _ => unreachable!(),
                        };
                        modifications[idx] = Some(stripped);
                        changes.push(OptimizationChange {
                            line: spanned.line,
                            description: format!("redundant feedrate F{f_val:.0} (already modal)"),
                        });
                    }
                }
            }
        }

        // ── Rule 7: consecutive same-axis travel ─────────────────────────────
        // When a new single-axis non-extruding move supersedes a prior one on
        // the same axis with the same feedrate, the prior move is redundant —
        // the printer will skip straight to the final position anyway.
        // Comments and Unknown commands are transparent (do not break the chain).
        //
        // Use the effective command here: if Rule 8 just stripped the feedrate
        // from this move, Rule 7 must compare against the stripped version so
        // it does not incorrectly collapse moves that differ only because one
        // carries a now-removed F parameter.
        let effective_cmd: &GCodeCommand<'_> = modifications[idx].as_ref().unwrap_or(cmd);
        if let Some((current_axis, current_feed)) = single_axis_travel(effective_cmd) {
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
            state.last_single_axis_travel = Some((
                idx,
                SingleAxisTravel {
                    axis: current_axis,
                    feedrate: current_feed,
                },
            ));
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
        // Filter redundant commands and apply in-place modifications (Rule 8).
        commands
            .into_iter()
            .enumerate()
            .filter_map(|(idx, mut spanned_cmd)| {
                if redundant[idx] {
                    None
                } else {
                    if let Some(modified) = modifications[idx].take() {
                        spanned_cmd.inner = modified;
                    }
                    Some(spanned_cmd)
                }
            })
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
// Pre-pass: collinear move merging (Rule 6)
// ─────────────────────────────────────────────────────────────────────────────

/// Maximum perpendicular distance (mm) for points to be considered collinear.
const COLLINEAR_TOLERANCE: f64 = 0.001;

/// Maximum relative deviation in extrusion rate per unit distance.
const EXTRUSION_TOLERANCE: f64 = 0.001;

/// Pre-pass: merge runs of collinear linear moves into single moves.
///
/// Detects three or more consecutive `LinearMove` commands lying on the same
/// 3D line with consistent feedrate and proportional extrusion, replacing the
/// entire run with a single move from the first point to the last.
///
/// Only runs when `config.merge_collinear` is `true`.
/// Respects `config.dry_run`: reports changes without modifying commands.
///
/// # Panics
///
/// Panics if internal invariants are violated (e.g. a run contains a
/// non-`LinearMove` command).  This indicates a logic bug in run detection.
#[must_use]
pub fn merge_collinear<'a>(
    commands: Vec<Spanned<GCodeCommand<'a>>>,
    config: &OptConfig,
) -> OptimizationResult<'a> {
    if !config.merge_collinear {
        return OptimizationResult {
            commands,
            changes: Vec::new(),
        };
    }

    // Identify mergeable runs (each run is a Vec of indices, length >= 3).
    let runs = find_collinear_runs(&commands);

    let mut changes: Vec<OptimizationChange> = Vec::new();

    // Record changes: every command in a run except the first is merged away.
    for run in &runs {
        for &idx in &run[1..] {
            changes.push(OptimizationChange {
                line: commands[idx].line,
                description: "collinear move merged into preceding run".to_owned(),
            });
        }
    }

    if config.dry_run {
        return OptimizationResult { commands, changes };
    }

    // Build per-index metadata: skip flag + optional rewrite target coords.
    let mut skip: Vec<bool> = vec![false; commands.len()];

    // Pre-extract the last command's coordinates for each run so we can
    // apply them when we consume the vec.
    let mut rewrite_target: Vec<Option<MoveAxes>> = vec![None; commands.len()];

    for run in &runs {
        let first = run[0];
        let last = *run.last().expect("run is non-empty");

        // Extract last command's x/y/z/e.
        if let GCodeCommand::LinearMove { x, y, z, e, .. } = &commands[last].inner {
            // Feedrate comes from the first command in the run.
            let first_feed = match &commands[first].inner {
                GCodeCommand::LinearMove { f, .. } => *f,
                _ => None,
            };
            rewrite_target[first] = Some(MoveAxes {
                x_pos: *x,
                y_pos: *y,
                z_pos: *z,
                extrude: *e,
                feed: first_feed,
            });
        }

        for &idx in &run[1..] {
            skip[idx] = true;
        }
    }

    let output: Vec<Spanned<GCodeCommand<'a>>> = commands
        .into_iter()
        .enumerate()
        .filter_map(|(idx, mut spanned)| {
            if skip[idx] {
                return None;
            }
            if let Some(target) = &rewrite_target[idx] {
                spanned.inner = GCodeCommand::LinearMove {
                    x: target.x_pos,
                    y: target.y_pos,
                    z: target.z_pos,
                    e: target.extrude,
                    f: target.feed,
                };
            }
            Some(spanned)
        })
        .collect();

    OptimizationResult {
        commands: output,
        changes,
    }
}

/// Resolved coordinates of a `LinearMove`, with missing axes defaulted to 0.
#[derive(Debug, Clone, Copy)]
struct LinearCoords {
    x: f64,
    y: f64,
    z: f64,
    e: Option<f64>,
    f: Option<f64>,
}

/// Extract `LinearMove` coordinates with defaults for missing axes.
/// Returns `None` for non-`LinearMove` commands.
fn linear_move_coords(cmd: &GCodeCommand<'_>) -> Option<LinearCoords> {
    if let GCodeCommand::LinearMove { x, y, z, e, f } = cmd {
        Some(LinearCoords {
            x: x.unwrap_or(0.0),
            y: y.unwrap_or(0.0),
            z: z.unwrap_or(0.0),
            e: *e,
            f: *f,
        })
    } else {
        None
    }
}

/// Feedrates match if both are `None`, or both are `Some` with values within
/// tolerance.
fn feedrates_match(a: Option<f64>, b: Option<f64>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(va), Some(vb)) => (va - vb).abs() < FEEDRATE_TOLERANCE,
        _ => false,
    }
}

/// Extrusion status matches: both `Some` or both `None`.
fn extrusion_status_matches(a: Option<f64>, b: Option<f64>) -> bool {
    a.is_some() == b.is_some()
}

/// Three floats representing a 3D position for collinearity checks.
#[derive(Debug, Clone, Copy)]
struct Vec3 {
    x: f64,
    y: f64,
    z: f64,
}

/// Perpendicular distance from point `c` to the line defined by `a`→`b`.
/// Uses the cross-product magnitude divided by |b-a|.
/// Returns `f64::INFINITY` if `a`==`b` (degenerate segment).
fn perpendicular_distance(a: Vec3, b: Vec3, c: Vec3) -> f64 {
    let ab = Vec3 {
        x: b.x - a.x,
        y: b.y - a.y,
        z: b.z - a.z,
    };
    let ab_len = (ab.x * ab.x + ab.y * ab.y + ab.z * ab.z).sqrt();
    if ab_len < 1e-12 {
        return f64::INFINITY; // degenerate: A==B
    }
    let ac = Vec3 {
        x: c.x - a.x,
        y: c.y - a.y,
        z: c.z - a.z,
    };
    // cross product AB x AC
    let cross = Vec3 {
        x: ab.y * ac.z - ab.z * ac.y,
        y: ab.z * ac.x - ab.x * ac.z,
        z: ab.x * ac.y - ab.y * ac.x,
    };
    let cross_len = (cross.x * cross.x + cross.y * cross.y + cross.z * cross.z).sqrt();
    cross_len / ab_len
}

/// Check that extrusion is proportional across all points in a run.
///
/// For points P0..Pn on a line, the extrusion at each point Pi should be
/// proportional to the distance from P0 to Pi relative to the total distance
/// P0 to Pn.  Returns `false` if any point deviates beyond tolerance.
fn extrusion_proportional(points: &[(f64, f64, f64, f64)]) -> bool {
    if points.len() < 2 {
        return true;
    }
    let (x0, y0, z0, e0) = points[0];
    let (xn, yn, zn, en) = *points.last().expect("checked len >= 2");

    let total_dist = ((xn - x0).powi(2) + (yn - y0).powi(2) + (zn - z0).powi(2)).sqrt();
    let total_e = en - e0;

    // If total distance is ~zero, all points are at the same location.
    // Not a meaningful line segment — reject.
    if total_dist < 1e-12 {
        return false;
    }

    for &(xi, yi, zi, ei) in &points[1..] {
        let dist_i = ((xi - x0).powi(2) + (yi - y0).powi(2) + (zi - z0).powi(2)).sqrt();
        let expected_e = e0 + total_e * (dist_i / total_dist);
        if (ei - expected_e).abs() > EXTRUSION_TOLERANCE {
            return false;
        }
    }

    true
}

/// Scan the command list and return groups of indices that form collinear runs
/// of 3+ `LinearMove` commands with matching feedrate and proportional extrusion.
fn find_collinear_runs(commands: &[Spanned<GCodeCommand<'_>>]) -> Vec<Vec<usize>> {
    let mut runs: Vec<Vec<usize>> = Vec::new();
    let mut current_run: Vec<usize> = Vec::new();

    for (idx, spanned) in commands.iter().enumerate() {
        let Some(coords) = linear_move_coords(&spanned.inner) else {
            // Non-LinearMove breaks any active run.
            flush_run(&mut current_run, commands, &mut runs);
            continue;
        };

        if current_run.is_empty() {
            current_run.push(idx);
            continue;
        }

        // Check compatibility with the current run.
        let first_idx = current_run[0];
        let first =
            linear_move_coords(&commands[first_idx].inner).expect("run start is a LinearMove");

        // Feedrate must match the first command in the run.
        if !feedrates_match(first.f, coords.f) {
            flush_run(&mut current_run, commands, &mut runs);
            current_run.push(idx);
            continue;
        }

        // Extrusion status must match (all Some or all None).
        if !extrusion_status_matches(first.e, coords.e) {
            flush_run(&mut current_run, commands, &mut runs);
            current_run.push(idx);
            continue;
        }

        // Collinearity: check that this point lies on the line from
        // the first point to the second point in the run (once we have >=2).
        if current_run.len() >= 2 {
            let a = linear_move_coords(&commands[current_run[0]].inner).expect("LinearMove");
            let b = linear_move_coords(&commands[current_run[1]].inner).expect("LinearMove");

            let va = Vec3 {
                x: a.x,
                y: a.y,
                z: a.z,
            };
            let vb = Vec3 {
                x: b.x,
                y: b.y,
                z: b.z,
            };
            let vc = Vec3 {
                x: coords.x,
                y: coords.y,
                z: coords.z,
            };

            if perpendicular_distance(va, vb, vc) > COLLINEAR_TOLERANCE {
                flush_run(&mut current_run, commands, &mut runs);
                current_run.push(idx);
                continue;
            }
        }

        current_run.push(idx);
    }

    flush_run(&mut current_run, commands, &mut runs);
    runs
}

/// If `current_run` has 3+ collinear moves with proportional extrusion,
/// push it to `runs`.  Always clears `current_run`.
fn flush_run(
    current_run: &mut Vec<usize>,
    commands: &[Spanned<GCodeCommand<'_>>],
    runs: &mut Vec<Vec<usize>>,
) {
    if current_run.len() >= 3 {
        // Final validation: extrusion proportionality across the entire run.
        let first_e = match &commands[current_run[0]].inner {
            GCodeCommand::LinearMove { e, .. } => *e,
            _ => None,
        };

        if first_e.is_some() {
            // All have Some(e) (checked during run building).
            let points: Vec<(f64, f64, f64, f64)> = current_run
                .iter()
                .map(|&idx| {
                    let c = linear_move_coords(&commands[idx].inner).expect("LinearMove");
                    (c.x, c.y, c.z, c.e.expect("extrusion status checked"))
                })
                .collect();

            if extrusion_proportional(&points) {
                runs.push(std::mem::take(current_run));
            }
        } else {
            // No extrusion — collinearity alone is sufficient.
            runs.push(std::mem::take(current_run));
        }
    }
    current_run.clear();
}

// ─────────────────────────────────────────────────────────────────────────────
// Post-pass: M73 progress marker insertion
// ─────────────────────────────────────────────────────────────────────────────

/// Result of the M73 progress marker insertion pass.
#[derive(Debug)]
pub struct ProgressInsertionResult<'a> {
    /// The command list with existing M73s stripped and new ones inserted at
    /// layer boundaries.
    pub commands: Vec<Spanned<GCodeCommand<'a>>>,
    /// One `I002` informational diagnostic per inserted M73 marker.
    pub diagnostics: Vec<crate::diagnostics::Diagnostic>,
}

/// Post-pass: strip existing M73 commands and insert recalculated progress
/// markers at layer boundaries (both `;LAYER_CHANGE` and Z-increase fallback).
///
/// Returns immediately with the original command list when
/// `config.insert_progress` is `false`.
///
/// Layer boundaries are detected via:
/// 1. A `Comment` whose text is exactly `"LAYER_CHANGE"` (slicer annotation).
/// 2. A `LinearMove` or `RapidMove` that increases the Z axis relative to the
///    last known Z value (fallback for files without layer-change comments).
///
/// When both signals are present in a file, only the comment-based signal fires
/// (the Z-increase fallback is suppressed after a `LAYER_CHANGE` comment is
/// seen for that boundary, preventing double-counting).
///
/// # Panics
///
/// Does not panic under any input.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn insert_progress_markers<'a>(
    commands: Vec<Spanned<GCodeCommand<'a>>>,
    estimated_time_seconds: f64,
    layer_count: u32,
    config: &OptConfig,
) -> ProgressInsertionResult<'a> {
    if !config.insert_progress {
        return ProgressInsertionResult {
            commands,
            diagnostics: Vec::new(),
        };
    }

    // Safety cap: never emit more markers than the known layer count.
    let effective_layer_count = layer_count.max(1);

    // ── Phase 1: strip existing M73 commands ─────────────────────────────────
    let stripped: Vec<Spanned<GCodeCommand<'a>>> = commands
        .into_iter()
        .filter(|s| !matches!(s.inner, GCodeCommand::MetaCommand { code: 73, .. }))
        .collect();

    // ── Phase 2: walk and insert at layer boundaries ──────────────────────────
    //
    // Two boundary signals:
    //   A) Comment text == "LAYER_CHANGE"
    //   B) Z-increase on a move (suppressed for the move immediately following
    //      a LAYER_CHANGE comment, to avoid double-counting)
    //
    // We build the output by appending commands one at a time and inserting a
    // synthetic M73 after the triggering command whenever a boundary fires.

    let mut output: Vec<Spanned<GCodeCommand<'a>>> = Vec::with_capacity(stripped.len() + effective_layer_count as usize);
    let mut diagnostics: Vec<Diagnostic> = Vec::new();

    let mut layers_seen: u32 = 0;
    // Last Z value seen from any move command.
    let mut last_z: Option<f64> = None;
    // True when the immediately-preceding real command was a LAYER_CHANGE
    // comment; in that case the next Z-increase does not trigger a second
    // boundary event.
    let mut layer_change_comment_pending = false;

    for spanned in stripped {
        let is_layer_change_comment = matches!(
            &spanned.inner,
            GCodeCommand::Comment { text } if text.as_ref() == "LAYER_CHANGE"
        );

        // Determine whether this command carries a new Z value.
        let move_z = match &spanned.inner {
            GCodeCommand::LinearMove { z, .. } | GCodeCommand::RapidMove { z, .. } => *z,
            _ => None,
        };

        // Determine trigger type before moving `spanned` into `output`.
        let trigger_line = spanned.line;
        let trigger_is_layer_change = is_layer_change_comment;

        // Check for Z-increase boundary (fallback signal).
        let z_increase_boundary = if let Some(z_new) = move_z {
            let is_increase = last_z.map_or(true, |z_old| z_new > z_old + 1e-9);
            if is_increase {
                // Suppress if a LAYER_CHANGE comment immediately preceded this move.
                !layer_change_comment_pending
            } else {
                false
            }
        } else {
            false
        };

        // Advance last_z before pushing so state is correct for following cmds.
        if let Some(z_new) = move_z {
            // Only advance if Z actually increased (we do not track retracts as layer changes).
            if last_z.map_or(true, |z_old| z_new > z_old + 1e-9) {
                last_z = Some(z_new);
            } else if last_z.map_or(true, |z_old| z_new >= z_old - 1e-9) {
                // Same or lower Z — still update last_z so we track current position.
                last_z = Some(z_new);
            }
        }

        output.push(spanned);

        // After pushing, reset the pending-comment flag if this was a real move.
        if move_z.is_some() {
            layer_change_comment_pending = false;
        }

        // Determine whether to insert a marker after this command.
        let should_insert = if trigger_is_layer_change {
            // Set flag so next Z-increase won't also fire.
            layer_change_comment_pending = true;
            true
        } else {
            z_increase_boundary
        };

        if should_insert && layers_seen < effective_layer_count {
            layers_seen += 1;

            // percent = layers_seen / layer_count * 100, capped at 100.
            let percent = ((f64::from(layers_seen) / f64::from(effective_layer_count)) * 100.0)
                .round()
                .min(100.0) as u32;

            // remaining_fraction is the proportion of time not yet elapsed.
            let elapsed_fraction =
                (f64::from(layers_seen) / f64::from(effective_layer_count)).min(1.0);
            let remaining_fraction = (1.0 - elapsed_fraction).max(0.0);
            let remaining_minutes =
                ((estimated_time_seconds * remaining_fraction) / 60.0).round() as u32;

            let params = format!("P{percent} R{remaining_minutes}");

            // Synthetic command: line 0, byte_offset 0 (no source location).
            let m73 = Spanned {
                inner: GCodeCommand::MetaCommand {
                    code: 73,
                    params: Cow::Owned(params.clone()),
                },
                line: 0,
                byte_offset: 0,
            };
            output.push(m73);

            diagnostics.push(Diagnostic {
                severity: Severity::Info,
                line: trigger_line,
                code: "I002",
                message: format!(
                    "inserted M73 {params} at layer {layers_seen}/{effective_layer_count}"
                ),
            });
        }
    }

    ProgressInsertionResult {
        commands: output,
        diagnostics,
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
        GCodeCommand::RapidMove {
            x: None,
            y: None,
            z: None,
            f: None,
        }
        | GCodeCommand::LinearMove {
            x: None,
            y: None,
            z: None,
            e: None,
            f: None,
        } => Some("empty move with no parameters".to_owned()),

        // ── Rule 4 + feedrate-only guard: moves with at least one field set ───
        GCodeCommand::RapidMove { x, y, z, f } => check_zero_delta_move(
            MoveAxes {
                x_pos: *x,
                y_pos: *y,
                z_pos: *z,
                extrude: None,
                feed: *f,
            },
            state,
        ),
        GCodeCommand::LinearMove { x, y, z, e, f } => check_zero_delta_move(
            MoveAxes {
                x_pos: *x,
                y_pos: *y,
                z_pos: *z,
                extrude: *e,
                feed: *f,
            },
            state,
        ),

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
        GCodeCommand::MetaCommand {
            code: code @ (104 | 109),
            params,
        } => {
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
        GCodeCommand::MetaCommand {
            code: code @ (140 | 190),
            params,
        } => {
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
        let x_ok = axes
            .x_pos
            .map_or(true, |v| (v - state.pos.x).abs() < POSITION_TOLERANCE);
        let y_ok = axes
            .y_pos
            .map_or(true, |v| (v - state.pos.y).abs() < POSITION_TOLERANCE);
        let z_ok = axes
            .z_pos
            .map_or(true, |v| (v - state.pos.z).abs() < POSITION_TOLERANCE);
        let e_ok = axes
            .extrude
            .map_or(true, |v| (v - state.pos.e).abs() < POSITION_TOLERANCE);

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

/// Tolerance for feedrate comparisons (mm/min).
/// Separate from `POSITION_TOLERANCE` to avoid semantic coupling.
const FEEDRATE_TOLERANCE: f64 = 0.000_1;

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
        GCodeCommand::RapidMove { x, y, z, f } => MoveAxes {
            x_pos: *x,
            y_pos: *y,
            z_pos: *z,
            extrude: None,
            feed: *f,
        },
        GCodeCommand::LinearMove { x, y, z, e, f } => MoveAxes {
            x_pos: *x,
            y_pos: *y,
            z_pos: *z,
            extrude: *e,
            feed: *f,
        },
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

    /// Modal feedrate for Rule 8 redundant feedrate elimination.
    /// `None` until the first `F` parameter is seen on any G0/G1 move.
    modal_feedrate: Option<f64>,
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
            modal_feedrate: None,
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
                self.apply_move(MoveAxes {
                    x_pos: *x,
                    y_pos: *y,
                    z_pos: *z,
                    extrude: None,
                    feed: *f,
                });
            }
            GCodeCommand::LinearMove { x, y, z, e, f } => {
                self.apply_move(MoveAxes {
                    x_pos: *x,
                    y_pos: *y,
                    z_pos: *z,
                    extrude: *e,
                    feed: *f,
                });
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

        // Track modal feedrate for Rule 8.
        match cmd {
            GCodeCommand::RapidMove { f: Some(val), .. }
            | GCodeCommand::LinearMove { f: Some(val), .. } => {
                self.modal_feedrate = Some(*val);
            }
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
        Spanned {
            inner: cmd,
            line,
            byte_offset: 0,
        }
    }

    fn g1_empty() -> GCodeCommand<'static> {
        GCodeCommand::LinearMove {
            x: None,
            y: None,
            z: None,
            e: None,
            f: None,
        }
    }

    fn g1_at(x: f64, y: f64) -> GCodeCommand<'static> {
        GCodeCommand::LinearMove {
            x: Some(x),
            y: Some(y),
            z: None,
            e: None,
            f: None,
        }
    }

    fn g1_f_only(f: f64) -> GCodeCommand<'static> {
        GCodeCommand::LinearMove {
            x: None,
            y: None,
            z: None,
            e: None,
            f: Some(f),
        }
    }

    fn g90() -> GCodeCommand<'static> {
        GCodeCommand::SetAbsolute
    }

    fn m106(params: &'static str) -> GCodeCommand<'static> {
        GCodeCommand::MetaCommand {
            code: 106,
            params: Cow::Borrowed(params),
        }
    }

    fn m104(params: &'static str) -> GCodeCommand<'static> {
        GCodeCommand::MetaCommand {
            code: 104,
            params: Cow::Borrowed(params),
        }
    }

    fn m1_misc(code: u16) -> GCodeCommand<'static> {
        GCodeCommand::MetaCommand {
            code,
            params: Cow::Borrowed(""),
        }
    }

    // ── Rule 1: empty G1 ─────────────────────────────────────────────────────

    #[test]
    fn empty_g1_is_removed() {
        let cmds = vec![spanned(g1_empty(), 1)];
        let result = optimize(cmds, &OptConfig::default());
        assert!(result.commands.is_empty(), "empty G1 should be removed");
        assert_eq!(result.changes.len(), 1);
        assert_eq!(
            result.changes[0].description,
            "empty move with no parameters"
        );
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
        let cmds = vec![spanned(g90(), 1), spanned(g90(), 2)];
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
            spanned(
                GCodeCommand::Comment {
                    text: Cow::Borrowed("a comment"),
                },
                2,
            ),
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
        let cmds = vec![spanned(m106("S255"), 1), spanned(m106("S255"), 2)];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(result.commands.len(), 1);
        assert_eq!(
            result.changes[0].description,
            "duplicate fan command (same setting already active)"
        );
    }

    #[test]
    fn m106_different_params_both_kept() {
        let cmds = vec![spanned(m106("S128"), 1), spanned(m106("S255"), 2)];
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
        assert_eq!(
            result.commands.len(),
            2,
            "second move to same pos should be removed"
        );
        assert_eq!(result.changes.len(), 1);
        assert_eq!(result.changes[0].line, 3);
    }

    #[test]
    fn feedrate_only_g1_not_removed() {
        // G1 F3000 — feedrate-only command; Rule 4 exception.
        let cmds = vec![spanned(g90(), 1), spanned(g1_f_only(3000.0), 2)];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(
            result.commands.len(),
            2,
            "feedrate-only G1 must be preserved"
        );
        assert!(result.changes.is_empty());
    }

    #[test]
    fn zero_delta_with_feedrate_not_removed() {
        // G1 X0 Y0 F3000 — moves to current position but changes feedrate.
        // The feedrate change is meaningful, so keep it.
        let cmds = vec![
            spanned(g90(), 1),
            spanned(
                GCodeCommand::LinearMove {
                    x: Some(0.0),
                    y: Some(0.0),
                    z: None,
                    e: None,
                    f: Some(3000.0),
                },
                2,
            ),
        ];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(
            result.commands.len(),
            2,
            "move with feedrate change must be kept"
        );
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
        assert_eq!(
            result.commands.len(),
            2,
            "zero move in relative mode must be kept"
        );
        assert!(result.changes.is_empty());
    }

    fn g0_x(x: f64) -> GCodeCommand<'static> {
        GCodeCommand::RapidMove {
            x: Some(x),
            y: None,
            z: None,
            f: None,
        }
    }

    fn g0_x_f(x: f64, f: f64) -> GCodeCommand<'static> {
        GCodeCommand::RapidMove {
            x: Some(x),
            y: None,
            z: None,
            f: Some(f),
        }
    }

    fn g0_y(y: f64) -> GCodeCommand<'static> {
        GCodeCommand::RapidMove {
            x: None,
            y: Some(y),
            z: None,
            f: None,
        }
    }

    fn g1_x_e(x: f64, e: f64) -> GCodeCommand<'static> {
        GCodeCommand::LinearMove {
            x: Some(x),
            y: None,
            z: None,
            e: Some(e),
            f: None,
        }
    }

    // ── Rule 7: consecutive same-axis travel merging ─────────────────────────

    #[test]
    fn test_rule7_same_axis_rapid_first_removed() {
        let cmds = vec![spanned(g0_x(10.0), 1), spanned(g0_x(20.0), 2)];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(result.commands.len(), 1, "first G0 X should be removed");
        assert_eq!(result.commands[0].line, 2);
        assert_eq!(result.changes.len(), 1);
    }

    #[test]
    fn test_rule7_different_axes_both_kept() {
        let cmds = vec![spanned(g0_x(10.0), 1), spanned(g0_y(20.0), 2)];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(result.commands.len(), 2, "different axes must both be kept");
        assert!(result.changes.is_empty());
    }

    #[test]
    fn test_rule7_extruding_moves_both_kept() {
        let cmds = vec![spanned(g1_x_e(10.0, 1.0), 1), spanned(g1_x_e(20.0, 2.0), 2)];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(
            result.commands.len(),
            2,
            "extruding moves must both be kept"
        );
        assert!(result.changes.is_empty());
    }

    #[test]
    fn test_rule7_different_feedrates_both_kept() {
        let cmds = vec![
            spanned(g0_x_f(10.0, 3000.0), 1),
            spanned(g0_x_f(20.0, 6000.0), 2),
        ];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(
            result.commands.len(),
            2,
            "different feedrates must both be kept"
        );
        assert!(result.changes.is_empty());
    }

    // ── Rule 8: redundant feedrate elimination ───────────────────────────

    #[test]
    fn test_rule8_redundant_feedrate_stripped() {
        let cmds = vec![
            spanned(
                GCodeCommand::LinearMove {
                    x: Some(10.0),
                    y: None,
                    z: None,
                    e: None,
                    f: Some(3000.0),
                },
                1,
            ),
            spanned(
                GCodeCommand::LinearMove {
                    x: Some(20.0),
                    y: None,
                    z: None,
                    e: None,
                    f: Some(3000.0),
                },
                2,
            ),
        ];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(result.commands.len(), 2, "both moves must survive");
        match &result.commands[0].inner {
            GCodeCommand::LinearMove { f, .. } => assert_eq!(*f, Some(3000.0)),
            other => panic!("expected LinearMove, got {other:?}"),
        }
        match &result.commands[1].inner {
            GCodeCommand::LinearMove { f, .. } => {
                assert_eq!(*f, None, "redundant F should be stripped")
            }
            other => panic!("expected LinearMove, got {other:?}"),
        }
        assert_eq!(result.changes.len(), 1);
    }

    #[test]
    fn test_rule8_first_feedrate_preserved() {
        let cmds = vec![spanned(
            GCodeCommand::LinearMove {
                x: Some(10.0),
                y: None,
                z: None,
                e: None,
                f: Some(3000.0),
            },
            1,
        )];
        let result = optimize(cmds, &OptConfig::default());
        match &result.commands[0].inner {
            GCodeCommand::LinearMove { f, .. } => {
                assert_eq!(*f, Some(3000.0), "first F must be preserved")
            }
            other => panic!("expected LinearMove, got {other:?}"),
        }
        assert!(result.changes.is_empty());
    }

    #[test]
    fn test_rule8_feedrate_change_preserved() {
        let cmds = vec![
            spanned(
                GCodeCommand::LinearMove {
                    x: Some(10.0),
                    y: None,
                    z: None,
                    e: None,
                    f: Some(3000.0),
                },
                1,
            ),
            spanned(
                GCodeCommand::LinearMove {
                    x: Some(20.0),
                    y: None,
                    z: None,
                    e: None,
                    f: Some(6000.0),
                },
                2,
            ),
        ];
        let result = optimize(cmds, &OptConfig::default());
        match &result.commands[1].inner {
            GCodeCommand::LinearMove { f, .. } => {
                assert_eq!(*f, Some(6000.0), "changed F must be preserved")
            }
            other => panic!("expected LinearMove, got {other:?}"),
        }
        assert!(result.changes.is_empty());
    }

    #[test]
    fn test_rule8_rapid_move_feedrate_stripped() {
        let cmds = vec![
            spanned(
                GCodeCommand::RapidMove {
                    x: Some(10.0),
                    y: None,
                    z: None,
                    f: Some(9000.0),
                },
                1,
            ),
            spanned(
                GCodeCommand::RapidMove {
                    x: Some(20.0),
                    y: None,
                    z: None,
                    f: Some(9000.0),
                },
                2,
            ),
        ];
        let result = optimize(cmds, &OptConfig::default());
        match &result.commands[1].inner {
            GCodeCommand::RapidMove { f, .. } => {
                assert_eq!(*f, None, "redundant F on G0 should be stripped")
            }
            other => panic!("expected RapidMove, got {other:?}"),
        }
    }

    #[test]
    fn test_rule8_no_feedrate_no_change() {
        let cmds = vec![spanned(g1_at(10.0, 20.0), 1), spanned(g1_at(30.0, 40.0), 2)];
        let result = optimize(cmds, &OptConfig::default());
        assert!(
            result.changes.is_empty(),
            "no F means no feedrate change to report"
        );
    }

    #[test]
    fn test_rule7_comment_between_still_removed() {
        let cmds = vec![
            spanned(g0_x(10.0), 1),
            spanned(
                GCodeCommand::Comment {
                    text: Cow::Borrowed("move"),
                },
                2,
            ),
            spanned(g0_x(20.0), 3),
        ];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(
            result.commands.len(),
            2,
            "first G0 X should be removed through comment"
        );
        let kept_lines: Vec<u32> = result.commands.iter().map(|s| s.line).collect();
        assert_eq!(kept_lines, vec![2, 3]);
    }

    #[test]
    fn test_rule7_multi_axis_move_not_matched() {
        let cmds = vec![
            spanned(
                GCodeCommand::RapidMove {
                    x: Some(10.0),
                    y: Some(20.0),
                    z: None,
                    f: None,
                },
                1,
            ),
            spanned(
                GCodeCommand::RapidMove {
                    x: Some(30.0),
                    y: Some(40.0),
                    z: None,
                    f: None,
                },
                2,
            ),
        ];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(
            result.commands.len(),
            2,
            "multi-axis moves must both be kept"
        );
    }

    // ── Rule 5: duplicate temperature command ─────────────────────────────────

    #[test]
    fn duplicate_m104_same_params_removed() {
        let cmds = vec![spanned(m104("S200"), 1), spanned(m104("S200"), 2)];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(result.commands.len(), 1);
        assert_eq!(
            result.changes[0].description,
            "duplicate temperature command (already set)"
        );
    }

    #[test]
    fn m104_different_temps_both_kept() {
        let cmds = vec![spanned(m104("S200"), 1), spanned(m104("S210"), 2)];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(result.commands.len(), 2);
        assert!(result.changes.is_empty());
    }

    // ── Dry-run mode ─────────────────────────────────────────────────────────

    #[test]
    fn dry_run_reports_changes_but_does_not_modify_commands() {
        let cmds = vec![spanned(g1_empty(), 1), spanned(g1_at(5.0, 5.0), 2)];
        let config = OptConfig {
            dry_run: true,
            ..Default::default()
        };
        let result = optimize(cmds, &config);

        // Command list is unchanged.
        assert_eq!(
            result.commands.len(),
            2,
            "dry-run must not remove any commands"
        );
        // But changes are still reported.
        assert_eq!(result.changes.len(), 1);
        assert_eq!(result.changes[0].line, 1);
    }

    // ── Mixed: 10 commands, 3 redundant → 7 in output ────────────────────────

    #[test]
    fn mixed_file_three_redundant_seven_kept() {
        let cmds = vec![
            spanned(g90(), 1),              // kept: first G90
            spanned(g90(), 2),              // REMOVED: duplicate G90
            spanned(g1_at(10.0, 10.0), 3),  // kept: actual move
            spanned(g1_at(10.0, 10.0), 4),  // REMOVED: zero-delta
            spanned(m104("S200"), 5),       // kept: first M104
            spanned(m104("S200"), 6),       // REMOVED: duplicate M104
            spanned(m106("S255"), 7),       // kept: first M106
            spanned(g1_at(20.0, 30.0), 8),  // kept: actual move
            spanned(m1_misc(84), 9),        // kept: M84 (motors off)
            spanned(g1_f_only(3000.0), 10), // kept: feedrate-only
        ];
        let result = optimize(cmds, &OptConfig::default());
        assert_eq!(
            result.commands.len(),
            7,
            "expected 7 commands after removing 3 redundant"
        );
        assert_eq!(
            result.changes.len(),
            3,
            "expected exactly 3 reported changes"
        );

        // Verify the surviving line numbers.
        let kept_lines: Vec<u32> = result.commands.iter().map(|s| s.line).collect();
        assert_eq!(kept_lines, vec![1, 3, 5, 7, 8, 9, 10]);
    }

    // ── Rule 6: collinear move merging ──────────────────────────────────────

    fn g1_xye(x: f64, y: f64, e: f64) -> GCodeCommand<'static> {
        GCodeCommand::LinearMove {
            x: Some(x),
            y: Some(y),
            z: None,
            e: Some(e),
            f: None,
        }
    }

    fn merge_config() -> OptConfig {
        OptConfig {
            dry_run: false,
            merge_collinear: true,
            ..Default::default()
        }
    }

    #[test]
    fn test_rule6_five_collinear_merge_to_one() {
        let cmds = vec![
            spanned(g1_xye(0.0, 0.0, 0.0), 1),
            spanned(g1_xye(1.0, 1.0, 1.0), 2),
            spanned(g1_xye(2.0, 2.0, 2.0), 3),
            spanned(g1_xye(3.0, 3.0, 3.0), 4),
            spanned(g1_xye(4.0, 4.0, 4.0), 5),
        ];
        let result = merge_collinear(cmds, &merge_config());
        assert_eq!(
            result.commands.len(),
            1,
            "5 collinear moves should merge to 1"
        );
        match &result.commands[0].inner {
            GCodeCommand::LinearMove { x, y, e, .. } => {
                assert!((x.unwrap() - 4.0).abs() < 1e-9);
                assert!((y.unwrap() - 4.0).abs() < 1e-9);
                assert!(
                    (e.unwrap() - 4.0).abs() < 1e-9,
                    "cumulative E must be preserved"
                );
            }
            other => panic!("expected LinearMove, got {other:?}"),
        }
        assert_eq!(result.changes.len(), 4, "4 moves merged away");
    }

    #[test]
    fn test_rule6_non_collinear_untouched() {
        let cmds = vec![
            spanned(g1_xye(0.0, 0.0, 0.0), 1),
            spanned(g1_xye(1.0, 0.0, 1.0), 2),
            spanned(g1_xye(1.0, 1.0, 2.0), 3),
        ];
        let result = merge_collinear(cmds, &merge_config());
        assert_eq!(result.commands.len(), 3, "non-collinear moves must be kept");
        assert!(result.changes.is_empty());
    }

    #[test]
    fn test_rule6_extrusion_mismatch_prevents_merge() {
        let cmds = vec![
            spanned(g1_xye(0.0, 0.0, 0.0), 1),
            spanned(g1_xye(1.0, 1.0, 1.0), 2),
            spanned(g1_xye(2.0, 2.0, 5.0), 3),
        ];
        let result = merge_collinear(cmds, &merge_config());
        assert!(
            result.commands.len() >= 2,
            "extrusion mismatch should prevent full merge"
        );
    }

    #[test]
    fn test_rule6_mixed_extrusion_status_prevents_merge() {
        let cmds = vec![
            spanned(g1_xye(0.0, 0.0, 0.0), 1),
            spanned(
                GCodeCommand::LinearMove {
                    x: Some(1.0),
                    y: Some(1.0),
                    z: None,
                    e: None,
                    f: None,
                },
                2,
            ),
            spanned(g1_xye(2.0, 2.0, 2.0), 3),
        ];
        let result = merge_collinear(cmds, &merge_config());
        assert_eq!(
            result.commands.len(),
            3,
            "mixed extrusion status must prevent merge"
        );
    }

    #[test]
    fn test_rule6_feedrate_mismatch_breaks_run() {
        let cmds = vec![
            spanned(
                GCodeCommand::LinearMove {
                    x: Some(0.0),
                    y: Some(0.0),
                    z: None,
                    e: Some(0.0),
                    f: Some(3000.0),
                },
                1,
            ),
            spanned(
                GCodeCommand::LinearMove {
                    x: Some(1.0),
                    y: Some(1.0),
                    z: None,
                    e: Some(1.0),
                    f: Some(3000.0),
                },
                2,
            ),
            spanned(
                GCodeCommand::LinearMove {
                    x: Some(2.0),
                    y: Some(2.0),
                    z: None,
                    e: Some(2.0),
                    f: Some(6000.0),
                },
                3,
            ),
        ];
        let result = merge_collinear(cmds, &merge_config());
        assert!(
            result.commands.len() >= 2,
            "feedrate change should break the run"
        );
    }

    #[test]
    fn test_rule6_disabled_by_default() {
        let cmds = vec![
            spanned(g1_xye(0.0, 0.0, 0.0), 1),
            spanned(g1_xye(1.0, 1.0, 1.0), 2),
            spanned(g1_xye(2.0, 2.0, 2.0), 3),
        ];
        let result = merge_collinear(cmds, &OptConfig::default());
        assert_eq!(
            result.commands.len(),
            3,
            "merge must be disabled by default"
        );
    }

    #[test]
    fn test_rule6_two_moves_not_merged() {
        let cmds = vec![
            spanned(g1_xye(0.0, 0.0, 0.0), 1),
            spanned(g1_xye(1.0, 1.0, 1.0), 2),
        ];
        let result = merge_collinear(cmds, &merge_config());
        assert_eq!(
            result.commands.len(),
            2,
            "fewer than 3 moves must not merge"
        );
    }

    #[test]
    fn test_rule6_preserves_extrusion_total() {
        let cmds = vec![
            spanned(g1_xye(0.0, 0.0, 0.0), 1),
            spanned(g1_xye(1.0, 1.0, 0.5), 2),
            spanned(g1_xye(2.0, 2.0, 1.0), 3),
            spanned(g1_xye(3.0, 3.0, 1.5), 4),
        ];
        let result = merge_collinear(cmds, &merge_config());
        match &result.commands[0].inner {
            GCodeCommand::LinearMove { e, .. } => {
                assert!(
                    (e.unwrap() - 1.5).abs() < 1e-6,
                    "merged E must equal last point's E"
                );
            }
            other => panic!("expected LinearMove, got {other:?}"),
        }
    }

    #[test]
    fn test_rule6_idempotent() {
        let cmds = vec![
            spanned(g1_xye(0.0, 0.0, 0.0), 1),
            spanned(g1_xye(1.0, 1.0, 1.0), 2),
            spanned(g1_xye(2.0, 2.0, 2.0), 3),
            spanned(g1_xye(3.0, 3.0, 3.0), 4),
        ];
        let pass1 = merge_collinear(cmds, &merge_config());
        let pass2 = merge_collinear(pass1.commands, &merge_config());
        assert_eq!(
            pass2.changes.len(),
            0,
            "second merge pass must produce zero changes"
        );
    }

    #[test]
    fn test_rule6_dry_run_no_modification() {
        let cmds = vec![
            spanned(g1_xye(0.0, 0.0, 0.0), 1),
            spanned(g1_xye(1.0, 1.0, 1.0), 2),
            spanned(g1_xye(2.0, 2.0, 2.0), 3),
        ];
        let config = OptConfig {
            dry_run: true,
            merge_collinear: true,
            ..Default::default()
        };
        let result = merge_collinear(cmds, &config);
        assert_eq!(result.commands.len(), 3, "dry-run must not modify commands");
        assert!(
            !result.changes.is_empty(),
            "dry-run must still report changes"
        );
    }

    // ── M73 progress marker insertion ────────────────────────────────────────

    fn progress_config() -> OptConfig {
        OptConfig {
            dry_run: false,
            merge_collinear: false,
            insert_progress: true,
        }
    }

    #[test]
    fn test_m73_inserted_at_layer_boundaries() {
        let cmds = vec![
            spanned(GCodeCommand::SetAbsolute, 1),
            spanned(
                GCodeCommand::Comment {
                    text: Cow::Borrowed("LAYER_CHANGE"),
                },
                2,
            ),
            spanned(
                GCodeCommand::LinearMove {
                    x: Some(10.0),
                    y: None,
                    z: Some(0.2),
                    e: Some(1.0),
                    f: Some(3000.0),
                },
                3,
            ),
            spanned(
                GCodeCommand::Comment {
                    text: Cow::Borrowed("LAYER_CHANGE"),
                },
                4,
            ),
            spanned(
                GCodeCommand::LinearMove {
                    x: Some(20.0),
                    y: None,
                    z: Some(0.4),
                    e: Some(2.0),
                    f: None,
                },
                5,
            ),
        ];
        let result = insert_progress_markers(cmds, 120.0, 2, &progress_config());
        let m73s: Vec<_> = result
            .commands
            .iter()
            .filter(|c| {
                matches!(
                    &c.inner,
                    GCodeCommand::MetaCommand { code: 73, .. }
                )
            })
            .collect();
        assert!(
            m73s.len() >= 2,
            "should insert M73 at each layer boundary, got {}",
            m73s.len()
        );
        let i002: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|d| d.code == "I002")
            .collect();
        assert_eq!(i002.len(), m73s.len());
    }

    #[test]
    fn test_m73_existing_stripped() {
        let cmds = vec![
            spanned(
                GCodeCommand::MetaCommand {
                    code: 73,
                    params: Cow::Borrowed("P50 R30"),
                },
                1,
            ),
            spanned(GCodeCommand::SetAbsolute, 2),
            spanned(
                GCodeCommand::Comment {
                    text: Cow::Borrowed("LAYER_CHANGE"),
                },
                3,
            ),
            spanned(
                GCodeCommand::LinearMove {
                    x: Some(10.0),
                    y: None,
                    z: Some(0.2),
                    e: Some(1.0),
                    f: Some(3000.0),
                },
                4,
            ),
        ];
        let result = insert_progress_markers(cmds, 60.0, 1, &progress_config());
        let old_m73s: Vec<_> = result
            .commands
            .iter()
            .filter(|c| match &c.inner {
                GCodeCommand::MetaCommand { code: 73, params } => params.as_ref() == "P50 R30",
                _ => false,
            })
            .collect();
        assert!(old_m73s.is_empty(), "existing M73 should be stripped");
    }

    #[test]
    fn test_m73_disabled_by_default() {
        let cmds = vec![
            spanned(
                GCodeCommand::Comment {
                    text: Cow::Borrowed("LAYER_CHANGE"),
                },
                1,
            ),
            spanned(
                GCodeCommand::LinearMove {
                    x: Some(10.0),
                    y: None,
                    z: Some(0.2),
                    e: Some(1.0),
                    f: Some(3000.0),
                },
                2,
            ),
        ];
        let result = insert_progress_markers(cmds, 60.0, 1, &OptConfig::default());
        assert!(result.diagnostics.is_empty());
        assert_eq!(result.commands.len(), 2);
    }

    #[test]
    fn test_m73_progress_spans_0_to_100() {
        let mut cmds = vec![spanned(GCodeCommand::SetAbsolute, 1)];
        for i in 0..4u32 {
            cmds.push(spanned(
                GCodeCommand::Comment {
                    text: Cow::Borrowed("LAYER_CHANGE"),
                },
                i * 2 + 2,
            ));
            cmds.push(spanned(
                GCodeCommand::LinearMove {
                    x: Some(10.0 * f64::from(i + 1)),
                    y: None,
                    z: Some(0.2 * f64::from(i + 1)),
                    e: Some(f64::from(i + 1)),
                    f: Some(3000.0),
                },
                i * 2 + 3,
            ));
        }
        let result = insert_progress_markers(cmds, 240.0, 4, &progress_config());
        let m73_params: Vec<String> = result
            .commands
            .iter()
            .filter_map(|c| match &c.inner {
                GCodeCommand::MetaCommand { code: 73, params } => Some(params.to_string()),
                _ => None,
            })
            .collect();
        assert!(!m73_params.is_empty());
        let last = m73_params.last().expect("at least one M73");
        assert!(last.contains("P100"), "last M73 should be P100, got {last}");
    }

    #[test]
    fn test_m73_z_increase_detection() {
        // No LAYER_CHANGE comments — relies on Z-increase fallback.
        let cmds = vec![
            spanned(GCodeCommand::SetAbsolute, 1),
            spanned(
                GCodeCommand::LinearMove {
                    x: Some(10.0),
                    y: None,
                    z: Some(0.2),
                    e: Some(1.0),
                    f: Some(3000.0),
                },
                2,
            ),
            spanned(
                GCodeCommand::LinearMove {
                    x: Some(20.0),
                    y: None,
                    z: Some(0.4),
                    e: Some(2.0),
                    f: None,
                },
                3,
            ),
        ];
        let result = insert_progress_markers(cmds, 60.0, 2, &progress_config());
        let m73s: Vec<_> = result
            .commands
            .iter()
            .filter(|c| {
                matches!(
                    &c.inner,
                    GCodeCommand::MetaCommand { code: 73, .. }
                )
            })
            .collect();
        assert!(
            m73s.len() >= 1,
            "should insert M73 at Z-based layer boundaries"
        );
    }
}
