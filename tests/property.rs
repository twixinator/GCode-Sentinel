//! Property-based tests using proptest.

use std::borrow::Cow;

use proptest::prelude::*;

use gcode_sentinel::analyzer::analyze;
use gcode_sentinel::dialect::{detect_dialect, Confidence};
use gcode_sentinel::models::{GCodeCommand, Spanned};
use gcode_sentinel::optimizer::{optimize, OptConfig};

/// Generate a finite f64 in a range (no NaN, no Infinity).
fn finite_f64(min: i64, max: i64) -> impl Strategy<Value = f64> {
    (min..=max).prop_map(|i| i as f64 / 100.0)
}

fn coord() -> impl Strategy<Value = f64> {
    finite_f64(0, 30000) // 0.0 – 300.0 mm
}

fn feedrate() -> impl Strategy<Value = f64> {
    finite_f64(10000, 1000000) // 100.0 – 10000.0 mm/min
}

fn e_value() -> impl Strategy<Value = f64> {
    finite_f64(-500, 5000) // -5.0 – 50.0 mm
}

fn arb_command() -> impl Strategy<Value = GCodeCommand<'static>> {
    prop_oneof![
        (
            prop::option::of(coord()),
            prop::option::of(coord()),
            prop::option::of(coord()),
            prop::option::of(feedrate()),
        )
            .prop_map(|(x, y, z, f)| GCodeCommand::RapidMove { x, y, z, f }),
        (
            prop::option::of(coord()),
            prop::option::of(coord()),
            prop::option::of(coord()),
            prop::option::of(e_value()),
            prop::option::of(feedrate()),
        )
            .prop_map(|(x, y, z, e, f)| GCodeCommand::LinearMove { x, y, z, e, f }),
        Just(GCodeCommand::SetAbsolute),
        Just(GCodeCommand::SetRelative),
        Just(GCodeCommand::Comment {
            text: Cow::Borrowed(" random test comment"),
        }),
        finite_f64(18000, 26000).prop_map(|t| GCodeCommand::MetaCommand {
            code: 104,
            params: Cow::Owned(format!("S{t}")),
        }),
    ]
}

fn arb_command_sequence() -> impl Strategy<Value = Vec<Spanned<GCodeCommand<'static>>>> {
    prop::collection::vec(arb_command(), 1..50).prop_map(|cmds| {
        let mut result = vec![Spanned {
            inner: GCodeCommand::SetAbsolute,
            line: 1,
            byte_offset: 0,
        }];
        for (i, cmd) in cmds.into_iter().enumerate() {
            result.push(Spanned {
                inner: cmd,
                line: (i + 2) as u32,
                byte_offset: 0,
            });
        }
        result
    })
}

proptest! {
    #[test]
    fn prop_extrusion_preserved_after_optimize(commands in arb_command_sequence()) {
        let pre = analyze(commands.iter(), None);
        let config = OptConfig::default();
        let opt_result = optimize(commands, &config);
        let post = analyze(opt_result.commands.iter(), None);
        let diff = (pre.stats.total_filament_mm - post.stats.total_filament_mm).abs();
        prop_assert!(
            diff < 1e-6,
            "Filament changed by {diff}: {:.6} -> {:.6}",
            pre.stats.total_filament_mm,
            post.stats.total_filament_mm
        );
    }

    #[test]
    fn prop_optimizer_converges(commands in arb_command_sequence()) {
        let config = OptConfig::default();
        let mut current = commands;
        // The optimizer must converge within a bounded number of passes.
        for pass in 0..10 {
            let result = optimize(current, &config);
            if result.changes.is_empty() {
                // Converged — property holds.
                return Ok(());
            }
            current = result.commands;
            prop_assert!(
                pass < 9,
                "Optimizer did not converge after 10 passes"
            );
        }
    }

    #[test]
    fn prop_mode_state_consistency_after_optimize(commands in arb_command_sequence()) {
        let config = OptConfig::default();
        let result = optimize(commands, &config);
        let mut is_absolute = true;
        for cmd in &result.commands {
            match &cmd.inner {
                GCodeCommand::SetAbsolute => is_absolute = true,
                GCodeCommand::SetRelative => is_absolute = false,
                GCodeCommand::LinearMove { x, y, z, .. }
                | GCodeCommand::RapidMove { x, y, z, .. } => {
                    if is_absolute {
                        if let Some(xv) = x { prop_assert!(*xv >= 0.0, "negative X in absolute mode at line {}", cmd.line); }
                        if let Some(yv) = y { prop_assert!(*yv >= 0.0, "negative Y in absolute mode at line {}", cmd.line); }
                        if let Some(zv) = z { prop_assert!(*zv >= 0.0, "negative Z in absolute mode at line {}", cmd.line); }
                    }
                }
                _ => {}
            }
        }
    }

    #[test]
    fn prop_dialect_returns_unknown_for_random_commands(commands in arb_command_sequence()) {
        let result = detect_dialect(&commands, None);
        prop_assert_eq!(
            result.metadata.dialect,
            gcode_sentinel::dialect::SlicerDialect::Unknown,
            "Expected Unknown dialect for random commands, got {:?}",
            result.metadata.dialect
        );
        prop_assert_eq!(result.metadata.confidence, Confidence::None);
    }

    #[test]
    fn prop_dialect_detection_is_deterministic(commands in arb_command_sequence()) {
        let r1 = detect_dialect(&commands, None);
        let r2 = detect_dialect(&commands, None);
        prop_assert_eq!(r1.metadata.dialect, r2.metadata.dialect);
        prop_assert_eq!(r1.metadata.confidence, r2.metadata.confidence);
        prop_assert_eq!(r1.metadata.slicer_version, r2.metadata.slicer_version);
        prop_assert_eq!(r1.metadata.nozzle_diameter_mm, r2.metadata.nozzle_diameter_mm);
    }
}
