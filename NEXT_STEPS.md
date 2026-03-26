# GCode-Sentinel -- Implementation Roadmap

**Baseline:** v1.0 complete (58 tests, 0 warnings). Parser, analyzer, optimizer (5 rules), emitter, CLI pipeline all functional.
**Date:** 2026-03-26
**Audience:** Contributors implementing these features.

---

## Version Overview Table

| Version | Theme | Milestone |
|---------|-------|-----------|
| **v2.0** | Foundations and Integration Testing | CI/CD, integration tests against real OrcaSlicer files, `--report-file`, JSON output |
| **v2.1** | Low-Risk Optimizer Extensions | Collinear merge, same-axis merge, redundant feedrate, M73 progress, min layer time advisory, temp tower detection |
| **v2.2** | Arc Fitting (G2/G3) | Highest-impact optimization: detect linear sequences on circular arcs and replace with arc commands |
| **v2.3** | Dialect Expansion | PrusaSlicer and Cura comment parsing, TOML machine profiles, `--machine` flag |
| **v3.0** | Travel Optimization | Island detection, nearest-neighbor reordering, post-optimization re-analysis validation |
| **v3.1** | Retraction Intelligence | Context-aware retraction removal, Z-hop optimization, wipe-before-retract detection |
| **v3.2** | Advanced Optimization | TSP 2-opt travel, combing, volumetric flow normalization, support structure analysis |

---

## v2.0 -- Foundations and Integration Testing

### Goal

Establish a trustworthy CI pipeline with integration tests against real OrcaSlicer G-Code, add machine-readable report output (`--report-file`, JSON), and lock down the post-optimization re-analysis safety net before any new optimizer rules land.

### Features

**Integration test harness** (new `tests/` directory)

- Create `tests/integration.rs` with `#[test]` functions that load `Orca GCODE/malm_slide.gcode` and `Orca GCODE/rose.gcode` via `include_str!` or runtime `std::fs::read_to_string` with a path relative to `CARGO_MANIFEST_DIR`.
- Tests use the library API directly (`parse_all`, `analyze`, `optimize`, `emit`) -- no subprocess spawning needed at this stage.
- **Complexity: Low.** This is test code, not production logic.

**Round-trip fidelity tests**

- For each real file: parse, emit to a `Vec<u8>`, re-parse the emitted output, and assert that every `GCodeCommand` variant and every numeric field matches within `f64::EPSILON` tolerance. Files affected: new `tests/integration.rs`.
- This catches parser/emitter drift and ensures lossless round-tripping for all OrcaSlicer constructs (thumbnail blocks, Klipper macros, packed params).
- **Complexity: Low.**

**Analyzer accuracy tests**

- Parse `malm_slide.gcode`, run `analyze`, and assert: `stats.layer_count == 255`, `stats.bbox_max.z` is approximately `51.05`, no `Severity::Error` diagnostics when limits are set to at least 300x300x400.
- Parse `rose.gcode`, run `analyze`, and assert: `stats.layer_count == 600`, `stats.bbox_max.z` is approximately `120.05`.
- Assert `total_filament_mm > 0.0` and `estimated_time_seconds > 0.0` for both files.
- **Complexity: Low.**

**Optimizer idempotence tests**

- For each real file: run `optimize` twice in succession. Assert the second pass produces zero `OptimizationChange` entries (all redundancies were removed on the first pass).
- **Complexity: Low.**

**Post-optimization re-analysis validation** (`src/main.rs`, `src/analyzer.rs`)

- After the optimizer runs, re-analyze the optimized command list. Compare the new `AnalysisResult` against the pre-optimization result. If any new `Severity::Error` diagnostic appears that was not present before, abort and report the regression.
- Add a new struct `ValidationDiff` to `src/diagnostics.rs` that holds the pre/post diagnostic delta.
- Wire the re-analysis into `main.rs` after the `optimize` call and before `write_output`.
- **Complexity: Medium.** The analyzer is already pure; the work is in the comparison and abort logic.

**`--report-file <path>` CLI flag** (`src/cli.rs`, `src/main.rs`, `src/diagnostics.rs`)

- New optional `--report-file` argument. When provided, write the `AnalysisReport` to the given path in addition to the stderr summary.
- Start with a plain-text format identical to the stderr output. JSON comes next.
- **Complexity: Low.**

**JSON report output** (`src/diagnostics.rs`, `src/main.rs`, `Cargo.toml`)

- Add `serde` (MIT OR Apache-2.0) and `serde_json` (MIT OR Apache-2.0) as dependencies.
- Derive `Serialize` on `AnalysisReport`, `PrintStats`, `Diagnostic`, `OptimizationChange`, `Severity`, and `Point3D`.
- Add `--report-format <text|json>` CLI flag (default `text`). When `json` is selected and `--report-file` is given, serialize to JSON. When `json` is selected without `--report-file`, write JSON to stdout instead of the G-Code output (mutually exclusive with non-`--check-only` mode).
- **Complexity: Low-Medium.** Serde derive is mechanical; the complexity is in the CLI argument validation (ensure `--report-format json` without `--report-file` implies `--check-only`).

**CI/CD with GitHub Actions** (new `.github/workflows/ci.yml`)

- Matrix: `stable` and `nightly` Rust on `ubuntu-latest` and `windows-latest`.
- Steps: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, `cargo test --release`.
- Cache `~/.cargo/registry` and `target/` directories.
- The workflow file references the test fixtures in `Orca GCODE/` which are committed to the repo.
- **Complexity: Low.**

### Integration Test Cases

| Test | File | Assertion |
|------|------|-----------|
| `round_trip_malm_slide` | `malm_slide.gcode` | Parse -> emit -> re-parse produces identical AST |
| `round_trip_rose` | `rose.gcode` | Parse -> emit -> re-parse produces identical AST |
| `analyze_malm_slide_layers` | `malm_slide.gcode` | `layer_count == 255`, `bbox_max.z ~ 51.05` |
| `analyze_rose_layers` | `rose.gcode` | `layer_count == 600`, `bbox_max.z ~ 120.05` |
| `analyze_no_errors_in_bounds` | both | Zero `Severity::Error` with 300x300x400 limits |
| `optimize_idempotent_malm` | `malm_slide.gcode` | Second optimize pass produces 0 changes |
| `optimize_idempotent_rose` | `rose.gcode` | Second optimize pass produces 0 changes |
| `optimize_preserves_extrusion` | both | `total_filament_mm` unchanged after optimize (within 1e-6) |
| `optimize_preserves_bbox` | both | `bbox_min` and `bbox_max` unchanged after optimize |
| `json_report_valid` | `malm_slide.gcode` | JSON output parses as valid JSON, contains `layer_count` field |
| `post_opt_reanalysis_no_regression` | both | No new `Error` diagnostics after optimization |

### AST / API Changes

- Add `#[derive(serde::Serialize)]` to `PrintStats`, `Diagnostic`, `OptimizationChange`, `Severity`, `AnalysisReport`, `Point3D` in `src/diagnostics.rs` and `src/models.rs`. These derives are additive and do not break existing API.
- Add `ValidationDiff` struct to `src/diagnostics.rs`:
  ```rust
  pub struct ValidationDiff {
      pub new_errors: Vec<Diagnostic>,
      pub resolved_errors: Vec<Diagnostic>,
      pub regression_detected: bool,
  }
  ```
- Add `--report-file`, `--report-format` to `Cli` in `src/cli.rs`.

### Safety Checklist

- [ ] All 58 existing unit tests still pass.
- [ ] `cargo clippy -- -D warnings` produces zero diagnostics.
- [ ] Round-trip tests confirm no data loss for both real G-Code files.
- [ ] `--check-only` mode still exits non-zero on error diagnostics.
- [ ] JSON output is valid JSON (tested by deserializing in the integration test).
- [ ] Post-optimization re-analysis does not false-positive on the two test files (the v1 optimizer rules should not introduce regressions).

### Dependencies

- None. This is the foundation version; all later versions depend on v2.0.

---

## v2.1 -- Low-Risk Optimizer Extensions

### Goal

Add six new optimizer and analyzer capabilities, all of which are low-risk, well-understood transformations or advisory-only analyses that do not reorder or restructure the G-Code command stream.

### Features

**Rule 6: Collinear move merging** (`src/optimizer.rs`) -- Complexity: Medium

- Detect three or more consecutive `LinearMove` commands on the same 3D line with consistent feedrate and linearly proportional extrusion.
- Collinearity test: cross product magnitude `|(B-A) x (C-A)| / |C-A|` below a configurable tolerance (default `0.001` mm).
- Extrusion proportionality test: interpolate the expected E at intermediate points; reject if deviation exceeds `0.001` mm.
- Merge the run into a single `LinearMove` from the first point to the last point, with the cumulative E value of the last command and the shared feedrate.
- Track state correctly: the optimizer's `PositionState` must skip intermediate positions for merged runs.
- Add `--merge-collinear` flag to `src/cli.rs` (opt-in, disabled by default). Wire into `OptConfig`.
- Golden-file test: create `tests/fixtures/collinear_input.gcode` and `tests/fixtures/collinear_expected.gcode`.

**Rule 7: Consecutive same-axis travel merging** (`src/optimizer.rs`) -- Complexity: Low

- Detect two consecutive `RapidMove` (or `LinearMove` with no E) commands that affect only the same single axis (e.g., `G0 X10` followed by `G0 X20`) with the same feedrate or no feedrate.
- Remove the first command; keep the second (the nozzle ends at the same destination).
- Only applies when neither command has an E parameter. Extruding moves are never merged by this rule.
- Add to the existing `check_rules` function as a new pattern match case.
- No new CLI flag needed; this is safe enough to run by default (same safety class as existing Rule 1/Rule 4).

**Rule 8: Redundant feedrate elimination** (`src/optimizer.rs`, `src/emitter.rs`) -- Complexity: Low

- This does not remove commands; it strips the `F` parameter from moves where the feedrate matches the modal (most recently set) feedrate.
- Implementation: add a `strip_redundant_feedrate` post-pass that mutates `LinearMove.f` and `RapidMove.f` to `None` when the value equals the tracked modal feedrate.
- Track modal feedrate as a new field in `PassState`. When a move specifies `F` and it matches the current modal value, set the field to `None` on the command.
- This requires a second pass after the main redundancy pass, or integration into the existing single pass with a post-processing step that modifies commands in place rather than removing them.
- No new CLI flag; this is universally safe across all firmware (feedrate is modal in all G-Code dialects).

**M73 progress marker insertion** (`src/optimizer.rs` or new `src/progress.rs`, `src/models.rs`, `src/emitter.rs`) -- Complexity: Low-Medium

- Add a `SetProgress` variant to `GCodeCommand` in `src/models.rs`:
  ```rust
  SetProgress {
      percent: Option<f64>,
      remaining_minutes: Option<f64>,
  },
  ```
- Add parser support: match `M73` in the `(b'M', 73)` arm of `parse_line_inner`, parse `P` and `R` parameters.
- Add emitter support: `M73 P{percent} R{remaining}`.
- The insertion pass runs after optimization and analysis: use `PrintStats.estimated_time_seconds` to compute cumulative progress, then insert `M73` commands at layer boundaries (after each `;LAYER_CHANGE` comment or Z-increase).
- Add `--insert-progress` CLI flag (opt-in). When enabled, strip any existing M73 commands first, then insert recalculated ones.
- Emit a diagnostic `I002: inserted M73 progress marker at layer N`.

**Minimum layer time advisory** (`src/analyzer.rs`, `src/diagnostics.rs`) -- Complexity: Medium

- During analysis, track per-layer print time. This requires accumulating time per layer between layer-change boundaries.
- Add `per_layer_times: Vec<f64>` to `PrintStats` (or a separate `LayerStats` struct).
- After analysis, scan `per_layer_times` for entries below a configurable threshold (default 10 seconds). Emit `W003: layer N print time {t:.1}s is below minimum {threshold}s` at `Severity::Warning`.
- No G-Code modification in v2.1 -- advisory only. Actual speed reduction deferred to v3+.
- Add `--min-layer-time <seconds>` CLI flag (default: disabled; when provided, enables the advisory).

**Temperature tower detection** (`src/analyzer.rs`, `src/diagnostics.rs`) -- Complexity: Low-Medium

- Scan the command list for `MetaCommand { code: 104 | 109, .. }` commands. Record each temperature change and the Z height at which it occurs.
- Detect a "staircase" pattern: 3 or more temperature changes at roughly regular Z intervals (tolerance: 20% of the interval between the first two changes), with temperatures in a monotonic or linear sequence.
- When detected, emit `I003: probable temperature tower detected` with the step table (Z range and temperature for each step).
- Also check for calibration hints in comment text: scan `Comment` commands for substrings `temperature tower`, `temp tower`, `calibration` (case-insensitive).
- Advisory only. No G-Code modification.

### Integration Test Cases

| Test | File | Assertion |
|------|------|-----------|
| `collinear_merge_reduces_commands` | synthetic golden file | Known 5-move collinear sequence merges to 1 move |
| `collinear_merge_preserves_extrusion` | both real files | `total_filament_mm` unchanged after collinear merge |
| `collinear_merge_preserves_non_collinear` | synthetic | Non-collinear moves are untouched |
| `collinear_extrusion_rate_mismatch_no_merge` | synthetic | Varying E/mm segments are not merged |
| `same_axis_travel_merge` | synthetic | `G0 X10` + `G0 X20` becomes `G0 X20` |
| `same_axis_no_merge_with_extrusion` | synthetic | `G1 X10 E1` + `G1 X20 E2` both kept |
| `redundant_feedrate_stripped` | both real files | Output file is smaller; re-parse produces identical analyzer results |
| `m73_insertion_at_layer_boundaries` | `malm_slide.gcode` | 255 M73 commands inserted, percentages span 0-100 |
| `min_layer_time_advisory_triggers` | synthetic (fast layer) | `W003` emitted for layer with < 10s print time |
| `temp_tower_detection_negative` | `malm_slide.gcode` | No `I003` diagnostic (not a temp tower) |
| `temp_tower_detection_positive` | synthetic temp tower | `I003` emitted with correct step table |

### AST / API Changes

- New `GCodeCommand::SetProgress { percent, remaining_minutes }` variant in `src/models.rs`.
- New parser arm `(b'M', 73)` in `src/parser.rs`.
- New emitter arm for `SetProgress` in `src/emitter.rs`.
- New `OptConfig` fields: `merge_collinear: bool`, `strip_redundant_feedrate: bool`.
- New `PrintStats` field: `per_layer_times: Vec<f64>` (or `LayerTimings` wrapper).
- New diagnostic codes: `I002` (M73 inserted), `I003` (temp tower detected), `W003` (short layer time).

### Safety Checklist

- [ ] All v2.0 tests still pass.
- [ ] Collinear merge preserves total extrusion within `1e-6` mm on both real files.
- [ ] Collinear merge preserves bounding box on both real files.
- [ ] Redundant feedrate stripping produces byte-identical analyzer results (same diagnostics, same stats) on both real files.
- [ ] M73 insertion does not alter any non-M73 command in the output.
- [ ] Post-optimization re-analysis detects no regressions from any new rule.
- [ ] `--check-only` mode is unaffected by new optimizer rules (they are only invoked when their flags are enabled or they are default-on safe rules).

### Dependencies

- v2.0 must be complete (integration test harness, post-optimization re-analysis, JSON output).

---

## v2.2 -- Arc Fitting (G2/G3)

### Goal

Implement the highest-impact single optimization: detect sequences of short `LinearMove` commands that approximate a circular arc and replace them with `G2` (clockwise) or `G3` (counter-clockwise) arc commands, reducing file size by 15-40% on curved geometry.

### Features

**AST extension: arc move variants** (`src/models.rs`) -- Complexity: Low

- Add two new variants to `GCodeCommand`:
  ```rust
  ArcMoveCW {
      x: Option<f64>,
      y: Option<f64>,
      z: Option<f64>,
      e: Option<f64>,
      f: Option<f64>,
      i: Option<f64>,
      j: Option<f64>,
  },
  ArcMoveCCW {
      x: Option<f64>,
      y: Option<f64>,
      z: Option<f64>,
      e: Option<f64>,
      f: Option<f64>,
      i: Option<f64>,
      j: Option<f64>,
  },
  ```
- The `R` (radius) format is intentionally omitted from the AST. The emitter always uses the I/J (center offset) format, which is unambiguous for arcs exceeding 180 degrees. If `R` format is needed later, it can be added to `EmitConfig`.

**Parser extension: G2/G3 parsing** (`src/parser.rs`) -- Complexity: Low

- Add `(b'G', 2)` and `(b'G', 3)` arms to the `match (letter, code)` block.
- Parse parameters X, Y, Z, E, F, I, J using a variant of the existing `parse_xyzef` helper (extend it or create `parse_arc_params` that also handles I and J letters).
- Existing files containing G2/G3 that were previously captured as `GCommand { code: 2, params: "..." }` will now be parsed into structured fields. This is a parse-behavior change; the `GCommand` fallback no longer fires for G2/G3. Ensure golden-file tests cover this.

**Emitter extension: G2/G3 serialization** (`src/emitter.rs`) -- Complexity: Low

- Add match arms for `ArcMoveCW` and `ArcMoveCCW`. Format: `G2 X{x} Y{y} I{i} J{j} E{e} F{f}` (omit absent fields).

**Analyzer extension: arc move simulation** (`src/analyzer.rs`) -- Complexity: Medium

- Arc moves update the print head position to `(x, y, z)` just like linear moves.
- The travel distance for an arc is the arc length, not the chord length. Compute: `arc_length = radius * |sweep_angle|`. Derive `radius` from I/J offsets: `radius = sqrt(i^2 + j^2)`. Derive `sweep_angle` from the start point, center, and end point using `atan2`.
- Update `update_move_stats` and `update_bbox` to sample points along the arc (the bounding box of an arc is not simply the endpoints; it includes axis-aligned extrema of the arc).
- Extrusion and retraction tracking apply identically to linear moves.

**Arc detection optimizer pass** (new `src/arc_fitter.rs`) -- Complexity: High

- Implement the sliding-window arc detection algorithm described in OPTIMIZATION_ROADMAP.md Section 3.2.
- Input: `Vec<Spanned<GCodeCommand>>`. Output: `Vec<Spanned<GCodeCommand>>` with qualifying G1 runs replaced by G2/G3 commands.
- **Circle fitting from 3 points:** Given consecutive points A, B, C, compute center (Ux, Uy) and radius R using the perpendicular bisector formula. When the determinant D is near zero (points are collinear), skip -- collinear merging (v2.1) handles that case.
- **Window extension:** If A, B, C fit a circle with max deviation below tolerance, try A, B, C, D. Continue extending as long as all points lie within tolerance of the fitted circle. Use least-squares circle fit for windows of 4+ points.
- **Consistency checks** (all must pass for a merge):
  - All moves in the window have the same feedrate (or no feedrate specified).
  - Extrusion rate (E per mm of arc length) is constant across all segments within 1% relative tolerance.
  - All moves are on the same Z plane (no Z change within the arc).
  - The window contains at least 3 original G1 moves.
  - The arc spans at least 15 degrees.
  - The fitted radius is between 0.5 mm and 1000 mm.
- **Direction detection:** Cross product of consecutive segment vectors determines CW (G2) vs CCW (G3).
- **Arc command construction:** Compute I/J offsets as center minus start point. Set X/Y to the endpoint of the arc. Set E to the cumulative extrusion of the merged segment. Set F to the shared feedrate (or omit).
- Add `--arc-fit` CLI flag (opt-in, disabled by default).
- Add `--arc-tolerance <mm>` CLI flag (default `0.01` mm).
- Register the arc fitter as a pass in the pipeline between the main optimizer and the emitter.

**Firmware compatibility warning** (`src/analyzer.rs` or `src/arc_fitter.rs`) -- Complexity: Low

- When `--arc-fit` is enabled, scan the file header (first 50 comment lines) for slicer identification. If the detected slicer or firmware dialect is known to have no or limited G2/G3 support (Sailfish, Bambu Lab), emit `W004: arc commands may not be supported by detected firmware/slicer`.
- This is a best-effort warning, not a blocker. The user explicitly opted in with `--arc-fit`.

### Integration Test Cases

| Test | File | Assertion |
|------|------|-----------|
| `arc_fit_quarter_circle` | synthetic: 10 G1 segments forming a quarter circle | Replaced by single G2 or G3; endpoint matches; I/J correct |
| `arc_fit_semicircle` | synthetic: 20 G1 segments forming a semicircle | Single arc command |
| `arc_fit_full_circle` | synthetic: 40 G1 segments forming full circle | Two semicircular arcs (full-circle split for firmware compat) |
| `arc_fit_s_curve` | synthetic: S-curve (CW then CCW) | Two arc commands with opposite directions |
| `arc_fit_collinear_rejected` | synthetic: 5 collinear G1 moves | No arc produced (handled by collinear merge instead) |
| `arc_fit_mixed_feedrate_rejected` | synthetic: circle with F change mid-sequence | Arc broken at feedrate change boundary |
| `arc_fit_extrusion_preserves_total` | both real files | `total_filament_mm` unchanged within `1e-6` |
| `arc_fit_bbox_preserved` | both real files | Bounding box unchanged (arc bbox includes extrema) |
| `arc_fit_round_trip` | synthetic G2/G3 file | Parse G2 -> emit -> re-parse produces identical AST |
| `arc_fit_rose_file_size_reduction` | `rose.gcode` | Output file is at least 5% smaller than input (rose has many curves) |
| `arc_fit_firmware_warning_sailfish` | synthetic with `; generated by Sailfish` header | `W004` diagnostic emitted |

### AST / API Changes

- New `GCodeCommand::ArcMoveCW` and `GCodeCommand::ArcMoveCCW` variants in `src/models.rs`.
- New parser arms `(b'G', 2)` and `(b'G', 3)` in `src/parser.rs`.
- New `parse_arc_params` helper in `src/parser.rs` (or extend `parse_xyzef` with I/J support).
- New emitter arms in `src/emitter.rs`.
- New `src/arc_fitter.rs` module, registered in `src/lib.rs`.
- New `OptConfig` field: `arc_fit: bool`, `arc_tolerance: f64`.
- New CLI flags: `--arc-fit`, `--arc-tolerance`.
- New diagnostic code: `W004` (firmware arc support warning).
- **Breaking change to existing behavior:** G2/G3 lines in input files will now parse as `ArcMoveCW`/`ArcMoveCCW` instead of `GCommand { code: 2|3, .. }`. Any downstream code matching on `GCommand` with code 2 or 3 must be updated. The emitter produces identical output, so round-trip behavior is preserved.

### Safety Checklist

- [ ] All v2.0 and v2.1 tests still pass.
- [ ] Arc fitting is disabled by default. No output changes unless `--arc-fit` is explicitly passed.
- [ ] Total extrusion is preserved within `1e-6` mm on both real files.
- [ ] Bounding box is preserved (arc bbox computation includes axis-aligned extrema).
- [ ] Post-optimization re-analysis detects no regressions.
- [ ] Files without curves (pure rectilinear geometry) are unmodified by the arc fitter.
- [ ] The parser correctly handles G2/G3 commands already present in input files (round-trip fidelity).
- [ ] The arc fitter never produces arcs with radius below 0.5 mm or above 1000 mm.
- [ ] The arc fitter never produces arcs spanning less than 15 degrees.

### Dependencies

- v2.1 must be complete (collinear merge handles the degenerate case where "arc" points are actually collinear).
- The post-optimization re-analysis from v2.0 is the safety net for this high-impact feature.

---

## v2.3 -- Dialect Expansion

### Goal

Extend GCode-Sentinel to parse and understand PrusaSlicer and Cura G-Code metadata comments, introduce TOML-based machine profile configuration files, and add a `--machine` CLI flag for selecting profiles.

### Features

**Slicer dialect detection** (new `src/dialect.rs`) -- Complexity: Medium

- Implement a `Dialect` enum:
  ```rust
  pub enum Dialect {
      OrcaSlicer,
      PrusaSlicer,
      SuperSlicer,
      Cura,
      Simplify3D,
      IdeaMaker,
      Unknown,
  }
  ```
- Implement `detect_dialect(commands: &[Spanned<GCodeCommand>]) -> Dialect` that scans the first 100 commands for slicer identification patterns:
  - `; generated by OrcaSlicer` -> `OrcaSlicer`
  - `; generated by PrusaSlicer` -> `PrusaSlicer`
  - `; generated by SuperSlicer` -> `SuperSlicer`
  - `; Generated with Cura_SteamEngine` -> `Cura`
  - `; G-Code generated by Simplify3D` -> `Simplify3D`
  - `; IdeaMaker` -> `IdeaMaker`
- Register as `pub mod dialect;` in `src/lib.rs`.

**PrusaSlicer metadata extraction** (`src/dialect.rs`) -- Complexity: Low-Medium

- PrusaSlicer and SuperSlicer embed metadata in comments at the end of the file (after `; filament used [mm] = ...`, `; total filament used [g] = ...`, `; estimated printing time (normal mode) = ...`).
- Also: `; nozzle_diameter = 0.4`, `; layer_height = 0.2`, `; filament_type = PLA`, `; temperature = 210`.
- Parse these into a `SlicerMetadata` struct:
  ```rust
  pub struct SlicerMetadata {
      pub dialect: Dialect,
      pub nozzle_diameter: Option<f64>,
      pub layer_height: Option<f64>,
      pub filament_type: Option<String>,
      pub hotend_temp: Option<f64>,
      pub bed_temp: Option<f64>,
      pub estimated_time: Option<f64>,
  }
  ```
- This metadata feeds into later features (volumetric flow checks, retraction advisories) but is also useful standalone for JSON report output.

**Cura metadata extraction** (`src/dialect.rs`) -- Complexity: Low-Medium

- Cura embeds settings in a header block: `;FLAVOR:Marlin`, `;Layer height: 0.2`, `;MINX:...`, `;MAXX:...`, etc.
- Cura uses `;LAYER:N` instead of `;LAYER_CHANGE`. The analyzer's layer detection logic in `handle_comment` must be extended to recognize this pattern.
- Parse Cura-specific comments into the same `SlicerMetadata` struct.

**Analyzer dialect awareness** (`src/analyzer.rs`) -- Complexity: Low

- Accept an optional `Dialect` or `SlicerMetadata` parameter in `analyze`.
- When dialect is `Cura`, treat `;LAYER:N` comments as layer change signals (in addition to `LAYER_CHANGE`).
- When dialect is `PrusaSlicer`, `;LAYER_CHANGE` is already supported (PrusaSlicer uses the same format as OrcaSlicer for this).

**TOML machine profiles** (new `src/machine.rs`, new `profiles/` directory) -- Complexity: Medium

- Add `toml` (MIT OR Apache-2.0) as a dependency in `Cargo.toml`.
- Define a `MachineProfile` struct:
  ```rust
  pub struct MachineProfile {
      pub name: String,
      pub max_x: f64,
      pub max_y: f64,
      pub max_z: f64,
      pub max_feedrate_xy: Option<f64>,
      pub max_feedrate_z: Option<f64>,
      pub acceleration: Option<f64>,
      pub nozzle_diameter: Option<f64>,
      pub firmware: Option<String>,  // "marlin", "klipper", "rrf"
  }
  ```
- Parse TOML files with the structure:
  ```toml
  [machine]
  name = "Creality Ender-3 V3"
  max_x = 220.0
  max_y = 220.0
  max_z = 250.0
  firmware = "marlin"

  [machine.optional]
  max_feedrate_xy = 500.0
  acceleration = 500.0
  nozzle_diameter = 0.4
  ```
- Ship 3-5 built-in profiles in a `profiles/` directory: `ender3.toml`, `prusa_mk4.toml`, `voron_v2.toml`, `bambu_x1c.toml`, `generic_300.toml`.
- `--machine` CLI flag loads a profile by name (searches `profiles/` directory, then `--config` path). Machine limits from the profile override defaults but are overridden by explicit `--max-x/y/z` flags.

**CLI `--machine` flag** (`src/cli.rs`, `src/main.rs`) -- Complexity: Low

- Add `--machine <name>` optional argument.
- Resolution order: explicit `--max-x/y/z` flags > `--machine` profile > `--config` file > defaults.
- If `--machine` is specified but the profile is not found, exit with a clear error listing available profiles.

### Integration Test Cases

| Test | File | Assertion |
|------|------|-----------|
| `detect_dialect_orcaslicer` | `malm_slide.gcode` | `Dialect::OrcaSlicer` detected |
| `detect_dialect_prusaslicer` | synthetic PrusaSlicer header | `Dialect::PrusaSlicer` detected |
| `detect_dialect_cura` | synthetic Cura header | `Dialect::Cura` detected |
| `cura_layer_detection` | synthetic Cura file with `;LAYER:N` | Layer count matches expected |
| `metadata_extraction_orcaslicer` | `malm_slide.gcode` | `SlicerMetadata` fields populated |
| `machine_profile_load` | `profiles/ender3.toml` | Loads without error; limits match expected |
| `machine_flag_overrides_defaults` | CLI test | `--machine ender3` sets limits to 220x220x250 |
| `explicit_flags_override_machine` | CLI test | `--machine ender3 --max-z 300` uses Z=300 |

### AST / API Changes

- New `src/dialect.rs` module with `Dialect`, `SlicerMetadata`, `detect_dialect`.
- New `src/machine.rs` module with `MachineProfile`, `load_profile`.
- New `toml` dependency in `Cargo.toml`.
- Extended `analyze` signature to accept optional `SlicerMetadata` (or pass it via a context struct).
- New CLI flags: `--machine <name>`.
- New `profiles/` directory with built-in TOML files.

### Safety Checklist

- [ ] All v2.0-v2.2 tests still pass.
- [ ] Dialect detection is never authoritative for safety decisions -- it only controls advisory output and metadata extraction. A misdetected dialect must not cause incorrect optimization.
- [ ] Machine profiles loaded from TOML are validated (no negative dimensions, no missing required fields).
- [ ] The `--machine` flag does not change any optimization behavior -- it only provides limits for the analyzer.
- [ ] PrusaSlicer and Cura files that happen to work with the OrcaSlicer parser (common G-Code subset) continue to parse correctly.

### Dependencies

- v2.0 must be complete (CI, integration tests, JSON output).
- v2.1 is recommended but not strictly required. Dialect detection is independent of optimizer extensions.

---

## v3.0 -- Travel Optimization

### Goal

Implement the first geometry-aware optimization: detect print islands within each layer and reorder them using a nearest-neighbor heuristic to minimize non-extruding travel distance, with post-optimization re-analysis as the mandatory safety gate.

### Features

**Layer segmentation** (new `src/layers.rs`) -- Complexity: Medium

- Build a `Layer` struct representing all commands within a single layer:
  ```rust
  pub struct Layer<'a> {
      pub index: u32,
      pub z_height: f64,
      pub commands: Vec<&'a Spanned<GCodeCommand<'a>>>,
      pub islands: Vec<Island>,
  }
  ```
- Segment the command list into layers using `;LAYER_CHANGE` comments (OrcaSlicer/PrusaSlicer) or `;LAYER:N` (Cura) or Z-increase detection as fallback.
- This module is the foundation for all geometry-aware features in v3.x.

**Island detection** (`src/layers.rs`) -- Complexity: Medium-High

- Within each layer, identify "islands" -- disconnected regions of extrusion separated by non-extruding travel moves.
- An island boundary is defined as: a `RapidMove` (G0) or a `LinearMove` (G1) with no E parameter, followed by one or more `LinearMove` commands with E > 0.
- Each `Island` stores:
  ```rust
  pub struct Island {
      pub entry_point: (f64, f64),   // XY of first extrusion move
      pub exit_point: (f64, f64),    // XY of last extrusion move
      pub command_range: Range<usize>,  // indices into Layer.commands
  }
  ```
- Islands with only 1-2 extrusion moves (e.g., a prime tower dot) are still valid islands.

**Nearest-neighbor island reordering** (new `src/travel_optimizer.rs`) -- Complexity: Medium

- For each layer with 2+ islands:
  1. Start at the nozzle's XY position at layer entry (exit point of the last command before the layer).
  2. Greedily select the island whose `entry_point` is closest (Euclidean distance) to the current position.
  3. After emitting that island's commands, update the current position to the island's `exit_point`.
  4. Repeat until all islands are visited.
- Regenerate travel moves (G0) between islands to reflect the new ordering.
- Preserve all commands within each island in their original order (perimeters, infill, etc. must not be reordered).
- Add `--optimize-travel` CLI flag (opt-in).

**Post-optimization re-analysis as hard gate** (`src/main.rs`) -- Complexity: Low (already built in v2.0)

- Travel optimization is the first feature that reorders commands. The re-analysis safety net from v2.0 is now critical.
- After travel optimization, re-analyze and compare:
  - `total_filament_mm` must be unchanged (within 1e-6).
  - No new `Severity::Error` diagnostics.
  - `layer_count` must be unchanged.
  - `bbox_min` and `bbox_max` must be unchanged.
- If any check fails, abort with a clear error message identifying which invariant was violated.

**Travel distance reporting** (`src/diagnostics.rs`) -- Complexity: Low

- Add `total_travel_mm: f64` to `PrintStats` (distance from non-extruding moves only, split from `total_distance_mm`).
- Report both travel and extrusion distance separately in the analysis report.
- After travel optimization, report the percentage reduction in travel distance.

### Integration Test Cases

| Test | File | Assertion |
|------|------|-----------|
| `layer_segmentation_malm` | `malm_slide.gcode` | 255 layers extracted, each with z_height increasing |
| `layer_segmentation_rose` | `rose.gcode` | 600 layers extracted |
| `island_detection_multi_island_layer` | synthetic (3 separate rectangles on one layer) | 3 islands detected with correct entry/exit points |
| `island_detection_single_island` | synthetic (one continuous perimeter) | 1 island, no reordering applied |
| `nearest_neighbor_reduces_travel` | synthetic (4 islands in worst-case order) | Travel distance reduced by at least 20% |
| `nearest_neighbor_preserves_extrusion` | both real files | `total_filament_mm` unchanged within 1e-6 |
| `nearest_neighbor_preserves_layer_count` | both real files | `layer_count` unchanged |
| `nearest_neighbor_preserves_bbox` | both real files | `bbox_min` and `bbox_max` unchanged |
| `reordering_within_island_preserved` | synthetic | Command order within each island is identical to original |
| `single_island_layer_unchanged` | synthetic | Layer with 1 island has identical output |

### AST / API Changes

- New `src/layers.rs` module with `Layer`, `Island`, `segment_layers`.
- New `src/travel_optimizer.rs` module with `reorder_islands_nearest_neighbor`.
- New `PrintStats` field: `total_travel_mm: f64`.
- New CLI flag: `--optimize-travel`.
- No new `GCodeCommand` variants needed.

### Safety Checklist

- [ ] All v2.x tests still pass.
- [ ] Travel optimization is disabled by default. No output changes unless `--optimize-travel` is passed.
- [ ] Total extrusion is preserved within `1e-6` mm.
- [ ] Layer count is preserved.
- [ ] Bounding box is preserved.
- [ ] No commands are lost or duplicated -- total command count per layer is unchanged.
- [ ] Commands within each island are in their original order.
- [ ] Post-optimization re-analysis gate runs automatically and aborts on regression.
- [ ] Layers with 0 or 1 islands are passed through unmodified.
- [ ] The travel optimizer does not modify any M-code, comment, or non-motion command.

### Dependencies

- v2.0 (post-optimization re-analysis is the mandatory safety net).
- v2.3 (dialect awareness for layer boundary detection -- `;LAYER_CHANGE` vs `;LAYER:N`).

---

## v3.1 -- Retraction Intelligence

### Goal

Implement context-aware retraction analysis and optimization: detect unnecessary retractions on short intra-island travels, optimize Z-hop usage, and provide wipe-before-retract advisories.

### Features

**Retraction event detection** (`src/analyzer.rs`, new `src/retraction.rs`) -- Complexity: Medium

- Build a `RetractionEvent` struct representing each retraction/prime cycle:
  ```rust
  pub struct RetractionEvent {
      pub retract_line: u32,
      pub prime_line: u32,
      pub retract_distance_mm: f64,
      pub travel_distance_mm: f64,
      pub travel_crosses_perimeter: bool, // requires island geometry
      pub z_hop: Option<f64>,
  }
  ```
- Detect retraction events: a negative E delta followed by zero-E travel moves followed by a positive E delta (the prime). Also detect firmware retraction: `G10` followed by `G11` (requires the `FirmwareRetract`/`FirmwareUnretract` AST variants -- add these now).
- Record all retraction events during analysis for downstream use.

**AST extension: firmware retraction** (`src/models.rs`, `src/parser.rs`, `src/emitter.rs`) -- Complexity: Low

- Add `FirmwareRetract` and `FirmwareUnretract` variants to `GCodeCommand`.
- Parser: `(b'G', 10)` -> `FirmwareRetract`, `(b'G', 11)` -> `FirmwareUnretract`.
- Emitter: `G10` and `G11` respectively.

**Context-aware retraction removal** (`src/retraction.rs`) -- Complexity: High

- For each retraction event, evaluate whether the retraction is necessary:
  1. **Distance check:** If `travel_distance_mm < threshold` (default 1.5 mm, configurable via `--retract-threshold`), the retraction is a candidate for removal.
  2. **Island check:** If the travel start and end points are within the same island (determined by `src/layers.rs` island data), the retraction is a stronger candidate for removal.
  3. **Perimeter crossing check (simplified):** If both the retract and prime positions are at infill-height Z and the travel distance is short, assume no perimeter crossing. Full polygon-based crossing detection is deferred to v3.2 (combing).
- When a retraction is deemed unnecessary:
  - Remove the retract move (negative E).
  - Remove the corresponding prime move (positive E that restores the E position).
  - If a Z-hop is associated, remove the Z-raise and Z-lower commands.
  - Adjust subsequent E values if the retraction removal changes the E position sequence.
- Add `--optimize-retractions` CLI flag (opt-in).
- **This is a high-risk optimization.** Always run post-optimization re-analysis.

**Z-hop optimization** (`src/retraction.rs`) -- Complexity: Medium

- Separate from retraction removal: identify Z-hop events that are unnecessary even when retraction is kept.
- Unnecessary Z-hop: travel distance < 1.0 mm within the same island, no tall features nearby.
- The "no tall features" check is simplified: if the travel Z is equal to the current layer Z and the travel is within the same island, Z-hop is unnecessary.
- Remove the Z-raise and Z-lower moves, convert to flat travel.
- Add `--optimize-zhop` CLI flag (opt-in, separate from `--optimize-retractions`).

**Wipe-before-retract advisory** (`src/retraction.rs`, `src/diagnostics.rs`) -- Complexity: Low

- Detect whether the slicer is performing wipe moves (a short non-extruding move along the just-printed perimeter immediately before retraction).
- Detection: a `LinearMove` with no E, distance < 5 mm, direction roughly aligned with the preceding extrusion move, immediately followed by a retraction.
- If wipe is not detected on perimeter-to-travel transitions, emit `W005: no wipe-before-retract detected on perimeter at line N`.
- Advisory only.

**Retraction distance advisory** (`src/retraction.rs`, `src/diagnostics.rs`) -- Complexity: Low

- Compare detected retraction distances against known-good ranges for the filament type (from `SlicerMetadata` if available).
- Emit `I004: retraction distance {d:.1}mm may be {high|low} for {filament_type}` when outside the expected range.
- Advisory only.

### Integration Test Cases

| Test | File | Assertion |
|------|------|-----------|
| `retraction_events_detected` | both real files | At least one `RetractionEvent` detected |
| `retraction_removal_short_travel` | synthetic (retract before 0.5mm travel within island) | Retract/prime pair removed |
| `retraction_kept_long_travel` | synthetic (retract before 50mm travel) | Retract/prime pair preserved |
| `retraction_removal_preserves_extrusion` | both real files | `total_filament_mm` unchanged within 1e-6 |
| `zhop_removal_short_travel` | synthetic (Z-hop before 0.3mm intra-island travel) | Z-raise/lower removed |
| `zhop_kept_long_travel` | synthetic (Z-hop before 20mm travel) | Z-raise/lower preserved |
| `wipe_detection_positive` | synthetic (wipe move present) | No `W005` emitted |
| `wipe_detection_negative` | synthetic (no wipe move) | `W005` emitted |
| `firmware_retraction_round_trip` | synthetic G10/G11 file | Parse -> emit -> re-parse identical |
| `post_opt_reanalysis_after_retraction_opt` | both real files | No new `Error` diagnostics |

### AST / API Changes

- New `GCodeCommand::FirmwareRetract` and `GCodeCommand::FirmwareUnretract` variants.
- New parser arms `(b'G', 10)` and `(b'G', 11)`.
- New emitter arms for the firmware retraction variants.
- New `src/retraction.rs` module with `RetractionEvent`, `analyze_retractions`, `optimize_retractions`, `optimize_zhops`.
- New CLI flags: `--optimize-retractions`, `--optimize-zhop`, `--retract-threshold <mm>`.
- New diagnostic codes: `W005` (no wipe detected), `I004` (retraction distance advisory).

### Safety Checklist

- [ ] All v2.x and v3.0 tests still pass.
- [ ] Retraction optimization is disabled by default.
- [ ] Total extrusion is preserved within `1e-6` mm.
- [ ] Post-optimization re-analysis gate runs and aborts on regression.
- [ ] Retraction removal never operates on extrusion moves (only on non-extruding travel sequences).
- [ ] Z-hop optimization never changes Z on extrusion moves.
- [ ] Firmware retraction (G10/G11) is handled correctly alongside G1 E-based retraction.
- [ ] The optimizer correctly handles the case where retraction removal changes the E sequence: all subsequent E values must remain consistent.

### Dependencies

- v3.0 (island detection and layer segmentation are needed for context-aware decisions).
- v2.3 (dialect detection for firmware retraction style: G10/G11 vs G1 E).

---

## v3.2 -- Advanced Optimization

### Goal

Implement the most sophisticated optimizations: TSP 2-opt refinement for travel, combing (travel within perimeters), volumetric flow normalization, and support structure analysis. These features share infrastructure from v3.0/v3.1 and require per-layer geometry understanding.

### Features

**TSP 2-opt travel refinement** (`src/travel_optimizer.rs`) -- Complexity: Medium-High

- After nearest-neighbor reordering (v3.0), apply 2-opt local search:
  1. For each pair of edges in the island tour, check if reversing the segment between them reduces total travel distance.
  2. If so, reverse that segment of the tour.
  3. Repeat until no improving swap is found (local optimum).
- Expected improvement over nearest-neighbor: 5-20% further travel reduction on complex layers.
- Add `--travel-optimizer <nearest-neighbor|2opt>` CLI flag (default `nearest-neighbor` when `--optimize-travel` is used).

**Combing (travel within perimeter)** (new `src/combing.rs`) -- Complexity: High

- Build a 2D polygon model of the current layer's perimeters from extrusion moves.
- For each travel move that crosses an exterior perimeter, compute an alternative path that stays inside the polygon.
- Use a simplified approach: identify the perimeter edges that the straight-line travel would cross, then route along the inner perimeter wall between the crossing points.
- When combing is possible, replace the straight-line travel with the combed path (a sequence of short G0 moves along the perimeter interior).
- Combing eliminates the need for retraction on the combed travel (the ooze stays inside the print).
- Add `--combing` CLI flag (opt-in, disabled by default).
- **This is the highest-complexity feature in the roadmap.** The polygon construction and path planning must be robust against concave polygons, holes (inner perimeters), and overlapping regions.
- Fallback: if the polygon model confidence is low (open perimeters, ambiguous geometry), skip combing for that travel and use straight-line with retraction.

**Volumetric flow normalization** (`src/analyzer.rs` or new `src/flow.rs`) -- Complexity: Medium

- Compute volumetric flow rate for each extrusion move: `flow = (E_delta / move_length) * filament_cross_section_area * speed`.
- Filament cross-section area from `SlicerMetadata.nozzle_diameter` and the layer height (or from `; filament_diameter` comment).
- Compare against configurable max flow (default 12 mm^3/s, configurable via `--max-flow <mm3/s>`).
- Advisory mode (default): emit `W006: volumetric flow {f:.1} mm^3/s exceeds limit {max:.1} mm^3/s at line N`.
- Auto-correction mode (`--normalize-flow`): reduce the feedrate on over-flow segments to bring flow within limits: `F_corrected = max_flow / (nozzle_width * layer_height)`. Recalculate and emit the corrected commands.
- Advisory mode is safe. Auto-correction mode requires post-optimization re-analysis.

**Support structure analysis (simplified)** (`src/analyzer.rs`) -- Complexity: Medium

- Simplified heuristic (not full polygon model): detect layers where the bounding box expands by more than 5 mm in any XY direction compared to the previous layer.
- Emit `W007: sudden bounding box expansion at layer N (X grew by {dx:.1}mm) -- possible unsupported overhang`.
- This is a coarse heuristic but catches obvious cases without requiring per-layer geometry infrastructure.
- Advisory only.
- Full polygon-based overhang detection is deferred to v4+ (see OPTIMIZATION_ROADMAP.md Section 9).

### Integration Test Cases

| Test | File | Assertion |
|------|------|-----------|
| `2opt_improves_over_nn` | synthetic (4+ islands in adversarial order) | 2-opt travel distance <= nearest-neighbor travel distance |
| `2opt_preserves_extrusion` | both real files | `total_filament_mm` unchanged |
| `combing_avoids_perimeter_crossing` | synthetic (travel that would cross outer wall) | Combed path stays inside polygon |
| `combing_fallback_on_open_perimeter` | synthetic (unclosed perimeter) | Straight-line travel used; no crash |
| `combing_preserves_extrusion` | both real files | `total_filament_mm` unchanged |
| `flow_advisory_triggers` | synthetic (move at 20 mm^3/s with max 12) | `W006` emitted |
| `flow_normalization_reduces_speed` | synthetic | Feedrate reduced; flow now within limit |
| `flow_normalization_preserves_extrusion` | synthetic | E values unchanged (only F is modified) |
| `support_heuristic_detects_expansion` | synthetic (bbox jumps 10mm) | `W007` emitted |
| `support_heuristic_normal_growth` | both real files | Minimal or no `W007` (normal gradual growth) |

### AST / API Changes

- New `src/combing.rs` module with `comb_travel`.
- New `src/flow.rs` module with `analyze_flow`, `normalize_flow`.
- New CLI flags: `--travel-optimizer <nearest-neighbor|2opt>`, `--combing`, `--max-flow <mm3/s>`, `--normalize-flow`.
- New diagnostic codes: `W006` (flow rate exceeded), `W007` (bbox expansion).
- No new `GCodeCommand` variants needed.

### Safety Checklist

- [ ] All previous version tests still pass.
- [ ] All new optimizations are opt-in (disabled by default).
- [ ] Combing fallback ensures no crash or data loss on ambiguous geometry.
- [ ] Flow normalization in auto-correct mode does not modify E values -- only F values.
- [ ] Post-optimization re-analysis gate runs for all modifying optimizations.
- [ ] TSP 2-opt produces travel distance <= nearest-neighbor (never worse).
- [ ] Support analysis is advisory only and never modifies commands.

### Dependencies

- v3.0 (layer segmentation, island detection, travel optimizer infrastructure).
- v3.1 (retraction analysis for combing integration -- combing removes the need for retraction on combed paths).
- v2.3 (slicer metadata for nozzle diameter, layer height used in flow calculations).

---

## Cross-Cutting Concerns

### Safety Invariants (Apply to Every Version)

These invariants must hold after any optimization pass. They are enforced by the post-optimization re-analysis gate introduced in v2.0.

1. **Extrusion preservation.** `total_filament_mm` must be unchanged within `1e-6` mm. Any optimization that alters total extrusion is a bug.
2. **Bounding box preservation.** `bbox_min` and `bbox_max` must not change (within `1e-3` mm) after optimization. Travel-only optimizations must not move the nozzle outside the original print volume.
3. **Layer count preservation.** `layer_count` must be unchanged. Optimizations must not merge or drop layers.
4. **No new errors.** The post-optimization analysis must not produce any `Severity::Error` diagnostic that was not present in the pre-optimization analysis.
5. **Command integrity.** No extrusion command (G1 with E) may be dropped by any optimization. Non-extruding commands may be removed, merged, or reordered only by their specific optimizer rules.
6. **Mode state consistency.** At every point in the optimized output, the positioning mode (G90/G91) and the extruder mode must be correct. The optimizer must not reorder commands in a way that changes the mode state at any point in the execution.

These invariants are checked programmatically in the integration tests. Any PR that introduces a new optimizer rule must add tests proving these invariants hold on both real G-Code files.

### Post-Optimization Re-Analysis Pipeline

Introduced in v2.0 and used by every subsequent version:

```
Input -> Parse -> Analyze(pre) -> Optimize -> Analyze(post) -> Diff(pre, post) -> Emit or Abort
```

The `Diff` step compares:
- `pre.stats.total_filament_mm` vs `post.stats.total_filament_mm`
- `pre.stats.layer_count` vs `post.stats.layer_count`
- `pre.stats.bbox_min/max` vs `post.stats.bbox_min/max`
- New `Severity::Error` diagnostics in `post` not present in `pre`

If any check fails, the tool prints the regression details to stderr and exits with a non-zero status code without writing the output file. This is the safety net that catches optimizer bugs before they reach hardware.

### Golden-File Test Strategy

Every optimization rule must have:

1. **Happy-path golden file:** A small synthetic `.gcode` input file and the expected `.gcode` output file, stored in `tests/fixtures/<rule_name>/`.
2. **No-op golden file:** An input where the rule does not apply; output must be byte-identical to input.
3. **Edge-case golden files:** Boundary conditions specific to the rule (documented in each version section above).
4. **Real-file invariant tests:** Run the optimizer on both real OrcaSlicer files and verify the safety invariants (extrusion, bbox, layer count).

Golden files are versioned in the repository. Any change to optimizer behavior that alters a golden file must be reviewed and the expected output updated explicitly.

### Fuzz Testing (v2.1+)

- Add `cargo-fuzz` (MIT OR Apache-2.0) as a dev dependency.
- Create fuzz targets for:
  - `fuzz_parse`: random byte sequences fed to `parse_line`. Must not panic.
  - `fuzz_round_trip`: random valid G-Code fed through parse -> emit -> re-parse. ASTs must match.
  - `fuzz_optimize`: random command sequences fed through `optimize`. Must not panic; safety invariants must hold.
- Fuzz targets live in `fuzz/` directory per `cargo-fuzz` convention.
- CI runs a short fuzz session (60 seconds per target) on every push.

### Property-Based Testing (v2.1+)

- Add `proptest` (MIT OR Apache-2.0) as a dev dependency.
- Property tests for:
  - **Extrusion preservation:** Generate random `Vec<GCodeCommand>`, optimize, assert `total_filament_mm` unchanged.
  - **Idempotence:** Optimize twice; second pass produces zero changes.
  - **Mode state:** At every command index in the optimized output, the modal state (absolute/relative, feedrate) is consistent with the command history up to that point.

### Diagnostic Code Registry

All diagnostic codes allocated across versions, for reference:

| Code | Severity | Description | Version |
|------|----------|-------------|---------|
| `E001` | Error | X exceeds machine limit | v1.0 |
| `E002` | Error | Y exceeds machine limit | v1.0 |
| `E003` | Error | Z exceeds machine limit | v1.0 |
| `W001` | Warning | Move to negative coordinate | v1.0 |
| `W002` | Warning/Info | Extruder retraction detected | v1.0 |
| `I001` | Info | Layer change detected (Z-based) | v1.0 |
| `I002` | Info | M73 progress marker inserted | v2.1 |
| `I003` | Info | Probable temperature tower detected | v2.1 |
| `W003` | Warning | Layer print time below minimum | v2.1 |
| `W004` | Warning | Arc commands may not be supported by firmware | v2.2 |
| `W005` | Warning | No wipe-before-retract detected on perimeter | v3.1 |
| `I004` | Info | Retraction distance advisory | v3.1 |
| `W006` | Warning | Volumetric flow exceeds limit | v3.2 |
| `W007` | Warning | Sudden bounding box expansion (possible overhang) | v3.2 |

### CLI Flag Summary (Cumulative)

| Flag | Version | Default | Description |
|------|---------|---------|-------------|
| `--input` | v1.0 | required | Input G-Code file |
| `--output` | v1.0 | stdout | Output file path |
| `--config` | v1.0 | none | TOML config path |
| `--max-x/y/z` | v1.0 | none | Machine axis limits |
| `--check-only` | v1.0 | false | Validate only, no output |
| `--verbose` | v1.0 | false | Debug output |
| `--report-file` | v2.0 | none | Write report to file |
| `--report-format` | v2.0 | text | Report format (text/json) |
| `--merge-collinear` | v2.1 | false | Enable collinear move merging |
| `--insert-progress` | v2.1 | false | Insert M73 progress markers |
| `--min-layer-time` | v2.1 | none | Minimum layer time advisory threshold (seconds) |
| `--arc-fit` | v2.2 | false | Enable arc fitting (G2/G3) |
| `--arc-tolerance` | v2.2 | 0.01 | Arc fitting tolerance (mm) |
| `--machine` | v2.3 | none | Machine profile name |
| `--optimize-travel` | v3.0 | false | Enable travel optimization |
| `--travel-optimizer` | v3.2 | nearest-neighbor | Travel optimizer algorithm |
| `--optimize-retractions` | v3.1 | false | Enable retraction optimization |
| `--optimize-zhop` | v3.1 | false | Enable Z-hop optimization |
| `--retract-threshold` | v3.1 | 1.5 | Retraction removal distance threshold (mm) |
| `--combing` | v3.2 | false | Enable combing |
| `--max-flow` | v3.2 | none | Maximum volumetric flow rate (mm^3/s) |
| `--normalize-flow` | v3.2 | false | Auto-correct over-flow segments |

### Required Team Skills

| Skill | First Needed | Versions |
|-------|-------------|----------|
| Rust (intermediate: traits, lifetimes, iterators) | v2.0 | All |
| CI/CD (GitHub Actions) | v2.0 | v2.0 |
| Computational geometry (collinearity, circle fitting) | v2.1 | v2.1, v2.2 |
| Numerical methods (least-squares fitting, atan2) | v2.2 | v2.2 |
| TOML parsing (serde + toml crate) | v2.3 | v2.3 |
| Graph algorithms (TSP heuristics, 2-opt) | v3.0 | v3.0, v3.2 |
| 2D polygon operations (point-in-polygon, path planning) | v3.2 | v3.2 |
| Fuzz testing (cargo-fuzz, libFuzzer) | v2.1 | v2.1+ |

### New Dependency Budget

| Crate | Version | License | First Needed | Purpose |
|-------|---------|---------|-------------|---------|
| `serde` | 1.x | MIT OR Apache-2.0 | v2.0 | Serialization derives |
| `serde_json` | 1.x | MIT OR Apache-2.0 | v2.0 | JSON report output |
| `toml` | 0.8.x | MIT OR Apache-2.0 | v2.3 | Machine profile parsing |
| `proptest` | 1.x (dev) | MIT OR Apache-2.0 | v2.1 | Property-based testing |
| `cargo-fuzz` | 0.12.x (dev) | MIT OR Apache-2.0 | v2.1 | Fuzz testing harness |

All dependencies use permissive licenses (MIT or Apache-2.0). No GPL, AGPL, SSPL, or copyleft dependencies are introduced.
