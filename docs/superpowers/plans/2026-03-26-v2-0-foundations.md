# v2.0 Foundations and Integration Testing — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Establish CI/CD, integration tests against real OrcaSlicer G-Code files, machine-readable JSON report output (`--report-file`, `--report-format`), and a post-optimization re-analysis safety gate that aborts on regressions.

**Architecture:** All new features are additive or wired into the existing `main.rs` pipeline. Serde derives are applied to existing types in `diagnostics.rs` and `models.rs`. `ValidationDiff` is a new struct in `diagnostics.rs`. CLI flags are added to `cli.rs`. Re-analysis and report file writes are wired in `main.rs` between the existing `optimize` call and `write_output`. Integration tests live in a new `tests/integration.rs` file that uses the library API directly.

**Tech Stack:** Rust 1.75+, Cargo, serde 1.x (derive feature), serde_json 1.x, GitHub Actions

---

## File Map

**Create:**
- `tests/integration.rs` — integration tests against real OrcaSlicer G-Code fixtures
- `.github/workflows/ci.yml` — CI pipeline: fmt, clippy, test on stable+nightly × ubuntu+windows

**Modify:**
- `Cargo.toml` — add `serde` (derive feature) and `serde_json` to `[dependencies]`
- `src/diagnostics.rs` — add `serde::Serialize` derives to all public types; add `ValidationDiff` struct with `compute` method
- `src/models.rs` — add `serde::Serialize` to `Point3D`
- `src/cli.rs` — add `ReportFormat` enum, `--report-file` and `--report-format` args to `Cli`
- `src/main.rs` — wire post-opt re-analysis abort, `write_report_file`, JSON stdout mode, updated imports

---

### Task 1: Add serde + serde_json to Cargo.toml

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add dependencies**

In `Cargo.toml`, under `[dependencies]`, add after the existing entries:

```toml
# JSON serialization for --report-format json — MIT OR Apache-2.0
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

- [ ] **Step 2: Verify resolution**

Run: `cargo check`
Expected: compiles without errors

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "feat: add serde and serde_json dependencies for JSON report output"
```

---

### Task 2: Add Serialize derives to core types

**Files:**
- Modify: `src/diagnostics.rs`
- Modify: `src/models.rs`

- [ ] **Step 1: Write failing unit tests**

At the bottom of `src/diagnostics.rs`, add a test module:

```rust
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
        };
        let json = serde_json::to_string(&report).unwrap();
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(val["stats"]["layer_count"].is_number());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test diagnostics::tests`
Expected: FAIL — `the trait bound ... Serialize is not satisfied` (compile error)

- [ ] **Step 3: Add Serialize to Point3D in models.rs**

In `src/models.rs`, change:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct Point3D {
```

to:

```rust
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Point3D {
```

- [ ] **Step 4: Add Serialize derives in diagnostics.rs**

Change each derive in `src/diagnostics.rs` as follows:

```rust
// Severity — was:
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
// becomes:
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub enum Severity {

// Diagnostic — was:
#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
// becomes:
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Diagnostic {

// PrintStats — was:
#[derive(Debug, Clone, PartialEq)]
pub struct PrintStats {
// becomes:
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PrintStats {

// OptimizationChange — was:
#[derive(Debug, Clone, PartialEq)]
pub struct OptimizationChange {
// becomes:
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct OptimizationChange {

// AnalysisReport — was:
#[derive(Debug, Clone)]
pub struct AnalysisReport {
// becomes:
#[derive(Debug, Clone, serde::Serialize)]
pub struct AnalysisReport {
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test diagnostics::tests`
Expected: PASS — both tests pass

- [ ] **Step 6: Run full suite to check for regressions**

Run: `cargo test`
Expected: all 58 existing tests pass plus 2 new tests

- [ ] **Step 7: Commit**

```bash
git add src/diagnostics.rs src/models.rs
git commit -m "feat: derive serde::Serialize on diagnostic and model types"
```

---

### Task 3: Add ValidationDiff struct to diagnostics.rs

**Files:**
- Modify: `src/diagnostics.rs`

- [ ] **Step 1: Write failing unit tests**

Append to the `#[cfg(test)] mod tests` block in `src/diagnostics.rs`:

```rust
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
```

- [ ] **Step 2: Run to verify tests fail**

Run: `cargo test diagnostics::tests`
Expected: FAIL — `ValidationDiff` not found (compile error)

- [ ] **Step 3: Add ValidationDiff struct and compute method**

In `src/diagnostics.rs`, after the closing `}` of the `impl AnalysisReport` block and before the `#[cfg(test)]` block, add:

```rust
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
        let pre_errors: Vec<&Diagnostic> =
            pre.iter().filter(|d| d.severity == Severity::Error).collect();
        let post_errors: Vec<&Diagnostic> =
            post.iter().filter(|d| d.severity == Severity::Error).collect();

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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test diagnostics::tests`
Expected: all 4 tests in the module pass

- [ ] **Step 5: Run full suite**

Run: `cargo test`
Expected: all tests pass

- [ ] **Step 6: Commit**

```bash
git add src/diagnostics.rs
git commit -m "feat: add ValidationDiff struct for post-optimization regression detection"
```

---

### Task 4: Add --report-file and --report-format CLI flags

**Files:**
- Modify: `src/cli.rs`

- [ ] **Step 1: Write failing unit tests**

At the bottom of `src/cli.rs`, add:

```rust
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
        let cli = Cli::try_parse_from([
            "gcode-sentinel",
            "input.gcode",
            "--report-format",
            "json",
        ])
        .unwrap();
        assert!(matches!(cli.report_format, ReportFormat::Json));
    }

    #[test]
    fn report_format_defaults_to_text() {
        let cli = Cli::try_parse_from(["gcode-sentinel", "input.gcode"]).unwrap();
        assert!(matches!(cli.report_format, ReportFormat::Text));
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test cli::tests`
Expected: FAIL — `report_file`, `ReportFormat` not found (compile error)

- [ ] **Step 3: Add ReportFormat enum**

In `src/cli.rs`, add `use clap::ValueEnum;` to the imports block, then add the enum before the `Cli` struct:

```rust
use clap::ValueEnum;
```

```rust
/// Output format for the analysis report written via `--report-file` or to
/// stdout when used without `--report-file`.
#[derive(Debug, Clone, PartialEq, ValueEnum)]
pub enum ReportFormat {
    /// Human-readable text, identical to the stderr summary (default).
    Text,
    /// Machine-readable JSON.
    Json,
}

impl Default for ReportFormat {
    fn default() -> Self {
        Self::Text
    }
}
```

- [ ] **Step 4: Add report_file and report_format fields to Cli**

Inside the `Cli` struct, append after the `verbose` field:

```rust
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
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test cli::tests`
Expected: all 3 tests pass

- [ ] **Step 6: Run full suite**

Run: `cargo test`
Expected: all tests pass

- [ ] **Step 7: Commit**

```bash
git add src/cli.rs
git commit -m "feat: add --report-file and --report-format CLI flags"
```

---

### Task 5: Wire post-opt re-analysis and report output in main.rs

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Replace the import block at the top of main.rs**

Replace the entire import section (lines 1–16) with:

```rust
#![warn(clippy::pedantic)]

use std::fs;
use std::io::Write as IoWrite;

use anyhow::{Context, Result};
use clap::Parser;
use memmap2::Mmap;
use tracing::{info, warn};

use gcode_sentinel::analyzer::analyze;
use gcode_sentinel::cli::{Cli, ReportFormat};
use gcode_sentinel::diagnostics::{AnalysisReport, OptimizationChange, Severity, ValidationDiff};
use gcode_sentinel::emitter::{emit, EmitConfig};
use gcode_sentinel::optimizer::{optimize, OptConfig};
use gcode_sentinel::parser::parse_all;
```

- [ ] **Step 2: Replace the main() function**

Replace the entire `main()` function with:

```rust
fn main() -> Result<()> {
    let cli = Cli::parse();

    init_tracing(cli.verbose)?;
    info!(input = %cli.input.display(), "starting GCode-Sentinel");

    validate_input(&cli)?;
    validate_cli_flags(&cli)?;

    let limits = cli.machine_limits();
    log_limits(limits.as_ref());

    let text = map_input(&cli)?;
    let commands = parse_all(&text).context("parse error in input file")?;
    info!(commands = commands.len(), "parse complete");

    let pre_analysis = analyze(commands.iter(), limits.as_ref());
    log_analysis(&pre_analysis.diagnostics);

    // JSON-to-stdout without --report-file implies check-only (no G-Code written).
    let effective_dry_run =
        cli.check_only || (cli.report_format == ReportFormat::Json && cli.report_file.is_none());
    let opt_config = OptConfig { dry_run: effective_dry_run };
    let opt_result = optimize(commands, &opt_config);
    log_optimization(opt_result.changes.len(), effective_dry_run);

    // Re-analyze the (possibly reduced) command list to detect regressions.
    let post_analysis = analyze(opt_result.commands.iter(), limits.as_ref());
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

    let report = build_report(post_analysis, opt_result.changes, effective_dry_run);
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
        write_output(&cli, &opt_result.commands)?;
    }

    Ok(())
}
```

- [ ] **Step 3: Add validate_cli_flags helper**

After the existing `validate_input` function, add:

```rust
fn validate_cli_flags(cli: &Cli) -> Result<()> {
    if cli.report_format == ReportFormat::Json
        && cli.report_file.is_none()
        && cli.output.is_some()
    {
        anyhow::bail!(
            "--report-format json without --report-file cannot be combined with --output \
             (JSON would go to stdout, conflicting with the G-Code output destination)"
        );
    }
    Ok(())
}
```

- [ ] **Step 4: Add write_report_file helper**

After the existing `print_report` function, add:

```rust
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
```

- [ ] **Step 5: Build and lint**

Run: `cargo build`
Expected: zero errors, zero warnings

Run: `cargo clippy -- -D warnings`
Expected: zero diagnostics

- [ ] **Step 6: Run full test suite**

Run: `cargo test`
Expected: all tests pass

- [ ] **Step 7: Commit**

```bash
git add src/main.rs
git commit -m "feat: wire post-opt re-analysis, ValidationDiff abort, and report file output in main"
```

---

### Task 6: Integration test harness

**Files:**
- Create: `tests/integration.rs`

- [ ] **Step 1: Create the integration test file**

Create `tests/integration.rs` with content:

```rust
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

    assert_eq!(result.stats.layer_count, 255, "malm_slide must have 255 layers");
    assert!(
        (result.stats.bbox_max.z - 51.05).abs() < 0.1,
        "malm_slide bbox_max.z must be ~51.05, got {}",
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

    assert_eq!(result.stats.layer_count, 600, "rose must have 600 layers");
    assert!(
        (result.stats.bbox_max.z - 120.05).abs() < 0.1,
        "rose bbox_max.z must be ~120.05, got {}",
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
            "{name}: expected zero errors within 300×300×400 bounds, got {error_count}"
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
            "{name}: total_filament_mm changed after optimization: {} → {}",
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
        255,
        "layer_count in JSON must be 255 for malm_slide"
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
```

- [ ] **Step 2: Run integration tests**

Run: `cargo test --test integration`
Expected: all 11 tests pass

If `analyze_malm_slide_layers` or `analyze_rose_layers` fail due to incorrect expected values, run:

```bash
cargo test --test integration analyze_malm_slide_layers -- --nocapture
```

Note the actual values from the failure message and update the assertions in `tests/integration.rs` to match.

- [ ] **Step 3: Run full suite**

Run: `cargo test`
Expected: all tests pass (58 existing unit tests + integration tests)

- [ ] **Step 4: Commit**

```bash
git add tests/integration.rs
git commit -m "feat: add integration tests against real OrcaSlicer G-Code fixtures"
```

---

### Task 7: GitHub Actions CI/CD workflow

**Files:**
- Create: `.github/workflows/ci.yml`

- [ ] **Step 1: Create the workflows directory**

Run: `mkdir -p .github/workflows`

- [ ] **Step 2: Create ci.yml**

Create `.github/workflows/ci.yml`:

```yaml
name: CI

on:
  push:
    branches: [main, master]
  pull_request:
    branches: [main, master]

env:
  CARGO_TERM_COLOR: always
  RUST_BACKTRACE: 1

jobs:
  test:
    name: Test (${{ matrix.rust }} on ${{ matrix.os }})
    runs-on: ${{ matrix.os }}
    strategy:
      fail-fast: false
      matrix:
        rust: [stable, nightly]
        os: [ubuntu-latest, windows-latest]

    steps:
      - name: Checkout repository
        uses: actions/checkout@v4

      - name: Install Rust toolchain (${{ matrix.rust }})
        uses: dtolnay/rust-toolchain@master
        with:
          toolchain: ${{ matrix.rust }}
          components: rustfmt, clippy

      - name: Cache Cargo registry and build artifacts
        uses: actions/cache@v4
        with:
          path: |
            ~/.cargo/registry
            ~/.cargo/git
            target
          key: ${{ runner.os }}-${{ matrix.rust }}-cargo-${{ hashFiles('**/Cargo.lock') }}
          restore-keys: |
            ${{ runner.os }}-${{ matrix.rust }}-cargo-

      - name: Check formatting
        run: cargo fmt --check

      - name: Clippy (deny warnings)
        run: cargo clippy -- -D warnings

      - name: Tests (debug)
        run: cargo test

      - name: Tests (release)
        run: cargo test --release
```

- [ ] **Step 3: Verify project still builds cleanly**

Run: `cargo build`
Expected: zero errors, zero warnings

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: add GitHub Actions workflow — stable/nightly × ubuntu/windows matrix"
```

---

## Self-Review

### Spec Coverage

| Requirement | Task |
|-------------|------|
| Integration test harness (`tests/integration.rs`) | Task 6 |
| Round-trip fidelity (`round_trip_malm_slide`, `round_trip_rose`) | Task 6 |
| Analyzer accuracy (`layer_count`, `bbox_max.z`, `total_filament_mm`, `estimated_time_seconds`) | Task 6 |
| Optimizer idempotence (both files) | Task 6 |
| `optimize_preserves_extrusion` and `optimize_preserves_bbox` | Task 6 |
| Post-optimization re-analysis validation | Tasks 3, 5 |
| `ValidationDiff` struct with `new_errors`, `resolved_errors`, `regression_detected` | Task 3 |
| Abort on regression in `main.rs` | Task 5 |
| `--report-file <path>` CLI flag | Tasks 4, 5 |
| `--report-format <text|json>` CLI flag | Tasks 4, 5 |
| Plain-text report file | Task 5 |
| JSON report file | Task 5 |
| JSON to stdout (without `--report-file`) | Task 5 |
| `serde` + `serde_json` dependencies | Task 1 |
| `serde::Serialize` on `Severity`, `Diagnostic`, `PrintStats`, `OptimizationChange`, `AnalysisReport`, `Point3D` | Tasks 2, 3 |
| CI/CD: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, `cargo test --release` | Task 7 |
| CI matrix: stable + nightly, ubuntu-latest + windows-latest | Task 7 |
| `json_report_valid` integration test | Task 6 |
| `post_opt_reanalysis_no_regression` integration test | Task 6 |
| All 11 integration test cases from the spec table | Task 6 |

### Placeholder Scan

No TBD, TODO, "implement later", or "similar to Task N" found.

### Type Consistency

- `ValidationDiff::compute(&[Diagnostic], &[Diagnostic])` — matches `analysis.diagnostics: Vec<Diagnostic>` at call sites ✓
- `ReportFormat` enum defined in `cli.rs` and imported in `main.rs` via `use gcode_sentinel::cli::{Cli, ReportFormat}` ✓
- `write_report_file(cli: &Cli, report: &AnalysisReport)` — `cli` is `&Cli`, `report` is `&AnalysisReport`, both match Task 5 call site ✓
- `AnalysisReport { diagnostics, stats, changes, dry_run }` constructor in Task 6 matches the struct definition from Task 2 ✓
- `ValidationDiff` imported via `use gcode_sentinel::diagnostics::ValidationDiff` in `tests/integration.rs` ✓
