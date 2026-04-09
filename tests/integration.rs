//! Integration tests against real OrcaSlicer G-Code fixtures.
//!
//! Tests use the library API directly — no subprocess spawning.
//! Fixtures live in `Orca GCODE/` at the repository root (note the space).

use std::fs;
use std::path::PathBuf;

use gcode_sentinel::analyzer::analyze;
use gcode_sentinel::arc_fitter::{fit_arcs, ArcFitConfig};
use gcode_sentinel::diagnostics::{AnalysisReport, Severity, ValidationDiff};
use gcode_sentinel::emitter::{emit, EmitConfig};
use gcode_sentinel::models::{GCodeCommand, MachineLimits};
use gcode_sentinel::optimizer::{insert_progress_markers, merge_collinear, optimize, OptConfig};
use gcode_sentinel::parser::parse_all;

fn fixture(name: &str) -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir).join("Orca GCODE").join(name)
}

// ── Round-trip fidelity ──────────────────────────────────────────────────────

#[test]
fn round_trip_malm_slide() {
    let text = fs::read_to_string(fixture("malm_slide.gcode"))
        .expect("fixture malm_slide.gcode must exist");
    let cmds = parse_all(&text).expect("malm_slide.gcode must parse");

    let mut buf1 = Vec::new();
    emit(&cmds, &mut buf1, &EmitConfig::default()).expect("first emit must succeed");

    let text2 = String::from_utf8(buf1.clone()).expect("emitted output must be valid UTF-8");
    let cmds2 = parse_all(&text2).expect("re-parsed output must parse");

    assert_eq!(
        cmds.len(),
        cmds2.len(),
        "command count must be identical after round-trip"
    );

    let mut buf2 = Vec::new();
    emit(&cmds2, &mut buf2, &EmitConfig::default()).expect("second emit must succeed");

    assert_eq!(
        buf1, buf2,
        "emitted output must be identical on second pass (round-trip fidelity)"
    );
}

#[test]
fn round_trip_rose() {
    let text = fs::read_to_string(fixture("rose.gcode")).expect("fixture rose.gcode must exist");
    let cmds = parse_all(&text).expect("rose.gcode must parse");

    let mut buf1 = Vec::new();
    emit(&cmds, &mut buf1, &EmitConfig::default()).expect("first emit must succeed");

    let text2 = String::from_utf8(buf1.clone()).expect("emitted output must be valid UTF-8");
    let cmds2 = parse_all(&text2).expect("re-parsed output must parse");

    assert_eq!(
        cmds.len(),
        cmds2.len(),
        "command count must be identical after round-trip"
    );

    let mut buf2 = Vec::new();
    emit(&cmds2, &mut buf2, &EmitConfig::default()).expect("second emit must succeed");

    assert_eq!(
        buf1, buf2,
        "emitted output must be identical on second pass (round-trip fidelity)"
    );
}

// ── Analyzer accuracy ────────────────────────────────────────────────────────

#[test]
fn analyze_malm_slide_layers() {
    let text = fs::read_to_string(fixture("malm_slide.gcode")).expect("fixture must exist");
    let cmds = parse_all(&text).expect("must parse");
    let result = analyze(cmds.iter(), None);

    assert_eq!(
        result.stats.layer_count, 255,
        "malm_slide must have 255 layers"
    );
    assert!(
        (result.stats.bbox_max.z - 51.45).abs() < 0.1,
        "malm_slide bbox_max.z must be ~51.45, got {}",
        result.stats.bbox_max.z
    );
    assert!(
        result.stats.total_filament_mm > 0.0,
        "total_filament_mm must be > 0"
    );
    assert!(
        result.stats.estimated_time_seconds > 0.0,
        "estimated_time_seconds must be > 0"
    );
}

#[test]
fn analyze_rose_layers() {
    let text = fs::read_to_string(fixture("rose.gcode")).expect("fixture must exist");
    let cmds = parse_all(&text).expect("must parse");
    let result = analyze(cmds.iter(), None);

    assert_eq!(result.stats.layer_count, 600, "rose must have 600 layers");
    assert!(
        (result.stats.bbox_max.z - 120.45).abs() < 0.1,
        "rose bbox_max.z must be ~120.45, got {}",
        result.stats.bbox_max.z
    );
    assert!(
        result.stats.total_filament_mm > 0.0,
        "total_filament_mm must be > 0"
    );
    assert!(
        result.stats.estimated_time_seconds > 0.0,
        "estimated_time_seconds must be > 0"
    );
}

#[test]
fn analyze_no_errors_in_bounds() {
    let limits = MachineLimits {
        max_x: 300.0,
        max_y: 300.0,
        max_z: 400.0,
    };

    for name in &["malm_slide.gcode", "rose.gcode"] {
        let text = fs::read_to_string(fixture(name))
            .unwrap_or_else(|_| panic!("fixture {name} must exist"));
        let cmds = parse_all(&text).unwrap_or_else(|_| panic!("{name} must parse"));
        let result = analyze(cmds.iter(), Some(&limits));

        let error_count = result
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count();
        assert_eq!(
            error_count, 0,
            "{name}: expected zero errors within 300x300x400 bounds, got {error_count}"
        );
    }
}

// ── Optimizer idempotence ────────────────────────────────────────────────────

#[test]
fn optimize_idempotent_malm() {
    let text = fs::read_to_string(fixture("malm_slide.gcode")).expect("fixture must exist");
    let cmds = parse_all(&text).expect("must parse");

    let config = OptConfig {
        dry_run: false,
        ..Default::default()
    };
    let pass1 = optimize(cmds, &config);
    let pass2 = optimize(pass1.commands, &config);

    assert_eq!(
        pass2.changes.len(),
        0,
        "second optimize pass on malm_slide must produce zero changes (idempotent)"
    );
}

#[test]
fn optimize_idempotent_rose() {
    let text = fs::read_to_string(fixture("rose.gcode")).expect("fixture must exist");
    let cmds = parse_all(&text).expect("must parse");

    let config = OptConfig {
        dry_run: false,
        ..Default::default()
    };
    let pass1 = optimize(cmds, &config);
    let pass2 = optimize(pass1.commands, &config);

    assert_eq!(
        pass2.changes.len(),
        0,
        "second optimize pass on rose must produce zero changes (idempotent)"
    );
}

// ── Optimizer preserves key metrics ─────────────────────────────────────────

#[test]
fn optimize_preserves_extrusion() {
    for name in &["malm_slide.gcode", "rose.gcode"] {
        let text = fs::read_to_string(fixture(name))
            .unwrap_or_else(|_| panic!("fixture {name} must exist"));
        let cmds = parse_all(&text).unwrap_or_else(|_| panic!("{name} must parse"));

        let pre = analyze(cmds.iter(), None);
        let opt = optimize(
            cmds,
            &OptConfig {
                dry_run: false,
                ..Default::default()
            },
        );
        let post = analyze(opt.commands.iter(), None);

        assert!(
            (pre.stats.total_filament_mm - post.stats.total_filament_mm).abs() < 1e-6,
            "{name}: total_filament_mm changed after optimization: {} -> {}",
            pre.stats.total_filament_mm,
            post.stats.total_filament_mm
        );
    }
}

#[test]
fn optimize_preserves_bbox() {
    for name in &["malm_slide.gcode", "rose.gcode"] {
        let text = fs::read_to_string(fixture(name))
            .unwrap_or_else(|_| panic!("fixture {name} must exist"));
        let cmds = parse_all(&text).unwrap_or_else(|_| panic!("{name} must parse"));

        let pre = analyze(cmds.iter(), None);
        let opt = optimize(
            cmds,
            &OptConfig {
                dry_run: false,
                ..Default::default()
            },
        );
        let post = analyze(opt.commands.iter(), None);

        assert_eq!(
            pre.stats.bbox_min, post.stats.bbox_min,
            "{name}: bbox_min changed after optimization"
        );
        assert_eq!(
            pre.stats.bbox_max, post.stats.bbox_max,
            "{name}: bbox_max changed after optimization"
        );
    }
}

// ── JSON output ──────────────────────────────────────────────────────────────

#[test]
fn json_report_valid() {
    let text = fs::read_to_string(fixture("malm_slide.gcode")).expect("fixture must exist");
    let cmds = parse_all(&text).expect("must parse");
    let analysis = analyze(cmds.iter(), None);
    let opt = optimize(cmds, &OptConfig::default());

    let report = AnalysisReport {
        diagnostics: analysis.diagnostics,
        stats: analysis.stats,
        changes: opt.changes,
        dry_run: false,
        slicer: None,
    };

    let json_str = serde_json::to_string(&report).expect("must serialize to JSON");
    let json_val: serde_json::Value =
        serde_json::from_str(&json_str).expect("serialized JSON must parse back");

    assert!(
        json_val["stats"]["layer_count"].is_u64(),
        "JSON must contain stats.layer_count as a number"
    );
    assert_eq!(
        json_val["stats"]["layer_count"].as_u64().unwrap(),
        255,
        "layer_count in JSON must be 255 for malm_slide"
    );
}

// ── Per-layer time tracking ──────────────────────────────────────────────────

#[test]
fn per_layer_times_match_layer_count_malm() {
    let text = fs::read_to_string(fixture("malm_slide.gcode")).expect("fixture must exist");
    let cmds = parse_all(&text).expect("must parse");
    let result = analyze(cmds.iter(), None);

    let diff = (result.stats.per_layer_times.len() as i64 - result.stats.layer_count as i64).abs();
    assert!(
        diff <= 1,
        "per_layer_times.len()={} should be within 1 of layer_count={}",
        result.stats.per_layer_times.len(),
        result.stats.layer_count,
    );
}

// ── Temperature tower guard ──────────────────────────────────────────────────

#[test]
fn no_temp_tower_in_malm_slide() {
    let text = fs::read_to_string(fixture("malm_slide.gcode")).expect("fixture must exist");
    let cmds = parse_all(&text).expect("must parse");
    let result = analyze(cmds.iter(), None);
    let i003: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|d| d.code == "I003")
        .collect();
    assert!(
        i003.is_empty(),
        "malm_slide is not a temp tower — I003 should not fire"
    );
}

// ── M73 progress marker insertion ────────────────────────────────────────────

#[test]
fn m73_insertion_malm_slide() {
    let text = fs::read_to_string(fixture("malm_slide.gcode")).expect("fixture must exist");
    let cmds = parse_all(&text).expect("must parse");
    let pre = analyze(cmds.iter(), None);

    let config = OptConfig {
        insert_progress: true,
        ..Default::default()
    };
    let opt = optimize(cmds, &OptConfig::default());
    let result = insert_progress_markers(
        opt.commands,
        pre.stats.estimated_time_seconds,
        pre.stats.layer_count,
        &pre.stats.per_layer_times,
        &config,
    );

    let m73_count = result
        .commands
        .iter()
        .filter(|c| {
            matches!(
                &c.inner,
                gcode_sentinel::models::GCodeCommand::MetaCommand { code: 73, .. }
            )
        })
        .count();

    assert_eq!(
        m73_count, 255,
        "malm_slide has 255 layers, should get 255 M73 markers"
    );
    assert_eq!(result.diagnostics.len(), 255);
}

// ── Post-optimization re-analysis ────────────────────────────────────────────

#[test]
fn post_opt_reanalysis_no_regression() {
    for name in &["malm_slide.gcode", "rose.gcode"] {
        let text = fs::read_to_string(fixture(name))
            .unwrap_or_else(|_| panic!("fixture {name} must exist"));
        let cmds = parse_all(&text).unwrap_or_else(|_| panic!("{name} must parse"));

        let pre = analyze(cmds.iter(), None);
        let opt = optimize(
            cmds,
            &OptConfig {
                dry_run: false,
                ..Default::default()
            },
        );
        let post = analyze(opt.commands.iter(), None);

        let diff = ValidationDiff::compute(&pre.diagnostics, &post.diagnostics);
        assert!(
            !diff.regression_detected,
            "{name}: optimizer introduced {} new error(s): {:?}",
            diff.new_errors.len(),
            diff.new_errors
        );
    }
}

// ── Full v2.1 pipeline ──────────────────────────────────────────────────────

#[test]
fn full_v21_pipeline_malm_slide() {
    let text = fs::read_to_string(fixture("malm_slide.gcode")).expect("fixture must exist");
    let cmds = parse_all(&text).expect("must parse");
    let pre = analyze(cmds.iter(), None);

    let config = OptConfig {
        dry_run: false,
        merge_collinear: true,
        insert_progress: true,
        ..Default::default()
    };

    let merged = merge_collinear(cmds, &config);
    let optimized = optimize(merged.commands, &config);
    let progress = insert_progress_markers(
        optimized.commands,
        pre.stats.estimated_time_seconds,
        pre.stats.layer_count,
        &pre.stats.per_layer_times,
        &config,
    );

    let post = analyze(progress.commands.iter(), None);
    let diff = ValidationDiff::compute(&pre.diagnostics, &post.diagnostics);

    assert!(
        !diff.regression_detected,
        "full v2.1 pipeline must not introduce regressions"
    );

    // Collinear merge consolidates E values across merged moves, which can
    // accumulate floating-point rounding error.  0.1mm tolerance is well
    // within acceptable limits for FDM printing (~0.001% relative error).
    assert!(
        (pre.stats.total_filament_mm - post.stats.total_filament_mm).abs() < 0.1,
        "extrusion must be preserved within tolerance: pre={} post={}",
        pre.stats.total_filament_mm,
        post.stats.total_filament_mm,
    );
}

#[test]
fn full_v21_pipeline_rose() {
    let text = fs::read_to_string(fixture("rose.gcode")).expect("fixture must exist");
    let cmds = parse_all(&text).expect("must parse");
    let pre = analyze(cmds.iter(), None);

    let config = OptConfig {
        dry_run: false,
        merge_collinear: true,
        insert_progress: true,
        ..Default::default()
    };

    let merged = merge_collinear(cmds, &config);
    let optimized = optimize(merged.commands, &config);
    let progress = insert_progress_markers(
        optimized.commands,
        pre.stats.estimated_time_seconds,
        pre.stats.layer_count,
        &pre.stats.per_layer_times,
        &config,
    );

    let post = analyze(progress.commands.iter(), None);
    let diff = ValidationDiff::compute(&pre.diagnostics, &post.diagnostics);

    assert!(!diff.regression_detected);
    // Collinear merge consolidates E values across merged moves, which can
    // accumulate floating-point rounding error.  0.1mm tolerance is well
    // within acceptable limits for FDM printing (~0.001% relative error).
    assert!(
        (pre.stats.total_filament_mm - post.stats.total_filament_mm).abs() < 0.1,
        "extrusion must be preserved within tolerance: pre={} post={}",
        pre.stats.total_filament_mm,
        post.stats.total_filament_mm,
    );
}

#[test]
fn feedrate_stripping_preserves_analysis() {
    for name in &["malm_slide.gcode", "rose.gcode"] {
        let text = fs::read_to_string(fixture(name))
            .unwrap_or_else(|_| panic!("fixture {name} must exist"));
        let cmds = parse_all(&text).unwrap_or_else(|_| panic!("{name} must parse"));

        let pre = analyze(cmds.iter(), None);
        let opt = optimize(cmds, &OptConfig::default());
        let post = analyze(opt.commands.iter(), None);

        assert!(
            (pre.stats.total_filament_mm - post.stats.total_filament_mm).abs() < 1e-6,
            "{name}: feedrate stripping must preserve filament total"
        );
        assert_eq!(
            pre.stats.layer_count, post.stats.layer_count,
            "{name}: feedrate stripping must preserve layer count"
        );
    }
}

// ── Arc fitting (v2.2) ───────────────────────────────────────────────────────

/// Enabled arc fitting on the quarter-circle fixture must produce at least one
/// G2/G3 command and preserve total extrusion within 0.1 mm.
#[test]
fn arc_fit_full_pipeline_obvious_arcs() {
    let text = fs::read_to_string(fixture("arc_quarter_circles.gcode"))
        .expect("fixture arc_quarter_circles.gcode must exist");
    let cmds = parse_all(&text).expect("must parse");

    let pre = analyze(cmds.iter(), None);
    let config = ArcFitConfig {
        enabled: true,
        tolerance_mm: 0.05,
    };
    let result = fit_arcs(cmds, &config);

    let arc_count = result
        .commands
        .iter()
        .filter(|c| {
            matches!(
                &c.inner,
                GCodeCommand::ArcMoveCW { .. } | GCodeCommand::ArcMoveCCW { .. }
            )
        })
        .count();
    assert!(
        arc_count >= 1,
        "expected at least one G2/G3 arc in the quarter-circle fixture, got {arc_count}"
    );
    assert!(
        !result.changes.is_empty(),
        "arc fitting must report at least one change"
    );

    let post = analyze(result.commands.iter(), None);
    assert!(
        (pre.stats.total_filament_mm - post.stats.total_filament_mm).abs() < 0.1,
        "arc fitting must preserve extrusion within 0.1 mm: pre={} post={}",
        pre.stats.total_filament_mm,
        post.stats.total_filament_mm,
    );
}

/// With arc fitting disabled, the command list is returned unchanged and the
/// emitted bytes must be identical to a straight parse → emit of the same file.
#[test]
fn arc_fit_disabled_passthrough() {
    let text = fs::read_to_string(fixture("arc_quarter_circles.gcode"))
        .expect("fixture arc_quarter_circles.gcode must exist");
    let cmds_a = parse_all(&text).expect("must parse (a)");
    let cmds_b = parse_all(&text).expect("must parse (b)");

    let disabled = ArcFitConfig {
        enabled: false,
        tolerance_mm: 0.02,
    };
    let result = fit_arcs(cmds_a, &disabled);

    assert!(
        result.changes.is_empty(),
        "disabled arc fitting must produce zero changes"
    );
    assert!(
        result.diagnostics.is_empty(),
        "disabled arc fitting must produce zero diagnostics"
    );

    let mut buf_original = Vec::new();
    emit(&cmds_b, &mut buf_original, &EmitConfig::default()).expect("emit original");

    let mut buf_passthrough = Vec::new();
    emit(
        &result.commands,
        &mut buf_passthrough,
        &EmitConfig::default(),
    )
    .expect("emit passthrough");

    assert_eq!(
        buf_original, buf_passthrough,
        "disabled arc fitting must not alter emitted output"
    );
}

/// The quarter-circle fixture has no slicer header comment.  Arc fitting must
/// produce at least one arc change, and W004 must appear exactly once.
#[test]
fn arc_fit_w004_warning_unknown_firmware() {
    let text = fs::read_to_string(fixture("arc_quarter_circles.gcode"))
        .expect("fixture arc_quarter_circles.gcode must exist");
    let cmds = parse_all(&text).expect("must parse");

    let config = ArcFitConfig {
        enabled: true,
        tolerance_mm: 0.05,
    };
    let result = fit_arcs(cmds, &config);

    assert!(
        !result.changes.is_empty(),
        "expected arc fitting to produce at least one change on arc_quarter_circles.gcode"
    );
    let w004_count = result
        .diagnostics
        .iter()
        .filter(|d| d.code == "W004")
        .count();
    assert_eq!(
        w004_count, 1,
        "expected exactly one W004 warning for unknown firmware, got {w004_count}"
    );
}

/// Running the collinear-merge pass followed by arc fitting on the combined
/// fixture must not introduce any validation regressions.
#[test]
fn arc_fit_combined_with_collinear_merge() {
    let text = fs::read_to_string(fixture("arc_with_collinear_prefix.gcode"))
        .expect("fixture arc_with_collinear_prefix.gcode must exist");
    let cmds = parse_all(&text).expect("must parse");

    let pre = analyze(cmds.iter(), None);

    let opt_config = OptConfig {
        dry_run: false,
        merge_collinear: true,
        ..Default::default()
    };
    let merged = merge_collinear(cmds, &opt_config);

    let arc_config = ArcFitConfig {
        enabled: true,
        tolerance_mm: 0.05,
    };
    let fitted = fit_arcs(merged.commands, &arc_config);

    let post = analyze(fitted.commands.iter(), None);
    let diff = ValidationDiff::compute(&pre.diagnostics, &post.diagnostics);

    assert!(
        !diff.regression_detected,
        "collinear merge + arc fitting must not introduce regressions: {:?}",
        diff.new_errors
    );
    assert!(
        (pre.stats.total_filament_mm - post.stats.total_filament_mm).abs() < 0.1,
        "combined passes must preserve extrusion within 0.1 mm: pre={} post={}",
        pre.stats.total_filament_mm,
        post.stats.total_filament_mm,
    );
}

/// Running arc fitting twice on already-fitted output must produce no further
/// changes (idempotence: G2/G3 commands are not LinearMove, so the sliding
/// window skips them entirely on the second pass).
#[test]
fn arc_fit_idempotent() {
    let text = fs::read_to_string(fixture("arc_quarter_circles.gcode"))
        .expect("fixture arc_quarter_circles.gcode must exist");
    let cmds = parse_all(&text).expect("must parse");

    let config = ArcFitConfig {
        enabled: true,
        tolerance_mm: 0.05,
    };
    let pass1 = fit_arcs(cmds, &config);

    // Second pass on already-converted commands.
    let pass2 = fit_arcs(pass1.commands, &config);

    assert_eq!(
        pass2.changes.len(),
        0,
        "second arc-fitting pass must produce zero changes (idempotent)"
    );
}

/// Arc fitting must not alter total extrusion when run on the existing real
/// fixtures (malm_slide, rose) which contain no arc-approximation sequences.
/// This guards against the pass accidentally corrupting non-arc G-Code.
#[test]
fn arc_fit_preserves_extrusion_on_existing_fixtures() {
    for name in &["malm_slide.gcode", "rose.gcode"] {
        let text = fs::read_to_string(fixture(name))
            .unwrap_or_else(|_| panic!("fixture {name} must exist"));
        let cmds = parse_all(&text).unwrap_or_else(|_| panic!("{name} must parse"));

        let pre = analyze(cmds.iter(), None);

        let config = ArcFitConfig {
            enabled: true,
            tolerance_mm: 0.02,
        };
        let result = fit_arcs(cmds, &config);

        let post = analyze(result.commands.iter(), None);

        assert!(
            (pre.stats.total_filament_mm - post.stats.total_filament_mm).abs() < 1e-6,
            "{name}: arc fitting must not alter total extrusion: pre={} post={}",
            pre.stats.total_filament_mm,
            post.stats.total_filament_mm,
        );

        let diff = ValidationDiff::compute(&pre.diagnostics, &post.diagnostics);
        assert!(
            !diff.regression_detected,
            "{name}: arc fitting must not introduce diagnostic regressions: {:?}",
            diff.new_errors
        );
    }
}

// ── Dialect detection ───────────────────────────────────────────────────────

#[test]
fn detect_dialect_malm_slide_is_orcaslicer() {
    let text = fs::read_to_string(fixture("malm_slide.gcode"))
        .expect("fixture malm_slide.gcode must exist");
    let cmds = parse_all(&text).expect("malm_slide.gcode must parse");

    let result = gcode_sentinel::dialect::detect_dialect(&cmds, None);
    assert_eq!(
        result.metadata.dialect,
        gcode_sentinel::dialect::SlicerDialect::OrcaSlicer
    );
    assert_eq!(
        result.metadata.confidence,
        gcode_sentinel::dialect::Confidence::High
    );
    assert!(
        result.metadata.slicer_version.is_some(),
        "OrcaSlicer version should be extracted"
    );
    assert_eq!(result.metadata.nozzle_diameter_mm, Some(0.4));
    assert_eq!(result.metadata.layer_height_mm, Some(0.2));
    assert_eq!(result.metadata.filament_type.as_deref(), Some("PLA"));
    assert_eq!(result.metadata.bed_temperature, Some(55.0));
    assert_eq!(result.metadata.hotend_temperature, Some(210.0));
    assert!(
        result.metadata.estimated_time_seconds.is_some(),
        "estimated time should be extracted"
    );
}

#[test]
fn detect_dialect_rose_is_orcaslicer() {
    let text = fs::read_to_string(fixture("rose.gcode")).expect("fixture rose.gcode must exist");
    let cmds = parse_all(&text).expect("rose.gcode must parse");

    let result = gcode_sentinel::dialect::detect_dialect(&cmds, None);
    assert_eq!(
        result.metadata.dialect,
        gcode_sentinel::dialect::SlicerDialect::OrcaSlicer
    );
    assert_eq!(
        result.metadata.confidence,
        gcode_sentinel::dialect::Confidence::High
    );
}
