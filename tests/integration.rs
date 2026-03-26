//! Integration tests against real OrcaSlicer G-Code fixtures.
//!
//! Tests use the library API directly — no subprocess spawning.
//! Fixtures live in `Orca GCODE/` at the repository root (note the space).

use std::fs;
use std::path::PathBuf;

use gcode_sentinel::analyzer::analyze;
use gcode_sentinel::diagnostics::{AnalysisReport, Severity, ValidationDiff};
use gcode_sentinel::emitter::{emit, EmitConfig};
use gcode_sentinel::models::MachineLimits;
use gcode_sentinel::optimizer::{optimize, OptConfig};
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

    assert_eq!(cmds.len(), cmds2.len(), "command count must be identical after round-trip");

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

    assert_eq!(cmds.len(), cmds2.len(), "command count must be identical after round-trip");

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

    assert_eq!(result.stats.layer_count, 1869, "malm_slide must have 1869 layers");
    assert!(
        (result.stats.bbox_max.z - 51.45).abs() < 0.1,
        "malm_slide bbox_max.z must be ~51.45, got {}",
        result.stats.bbox_max.z
    );
    assert!(result.stats.total_filament_mm > 0.0, "total_filament_mm must be > 0");
    assert!(result.stats.estimated_time_seconds > 0.0, "estimated_time_seconds must be > 0");
}

#[test]
fn analyze_rose_layers() {
    let text = fs::read_to_string(fixture("rose.gcode")).expect("fixture must exist");
    let cmds = parse_all(&text).expect("must parse");
    let result = analyze(cmds.iter(), None);

    assert_eq!(result.stats.layer_count, 4629, "rose must have 4629 layers");
    assert!(
        (result.stats.bbox_max.z - 120.45).abs() < 0.1,
        "rose bbox_max.z must be ~120.45, got {}",
        result.stats.bbox_max.z
    );
    assert!(result.stats.total_filament_mm > 0.0, "total_filament_mm must be > 0");
    assert!(result.stats.estimated_time_seconds > 0.0, "estimated_time_seconds must be > 0");
}

#[test]
fn analyze_no_errors_in_bounds() {
    let limits = MachineLimits { max_x: 300.0, max_y: 300.0, max_z: 400.0 };

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

    let config = OptConfig { dry_run: false };
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

    let config = OptConfig { dry_run: false };
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
        let opt = optimize(cmds, &OptConfig { dry_run: false });
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
        let opt = optimize(cmds, &OptConfig { dry_run: false });
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
        1869,
        "layer_count in JSON must be 1869 for malm_slide"
    );
}

// ── Post-optimization re-analysis ────────────────────────────────────────────

#[test]
fn post_opt_reanalysis_no_regression() {
    for name in &["malm_slide.gcode", "rose.gcode"] {
        let text = fs::read_to_string(fixture(name))
            .unwrap_or_else(|_| panic!("fixture {name} must exist"));
        let cmds = parse_all(&text).unwrap_or_else(|_| panic!("{name} must parse"));

        let pre = analyze(cmds.iter(), None);
        let opt = optimize(cmds, &OptConfig { dry_run: false });
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
