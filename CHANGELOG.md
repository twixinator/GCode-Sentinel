# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Breaking changes

- `gcode_sentinel::arc_fitter::arc_span` is no longer part of the public API.
  The function has been moved to the internal `geometry` module
  (`pub(crate)`) as part of the H2 deduplication. Downstream code that
  called `arc_span` directly must be updated to replicate the logic locally.

## [0.2.1] - 2026-03-31

### Fixed

- **Rule 7 absolute-mode guard**: Rule 7 (same-axis travel merging) is now
  skipped entirely when any `G91` (relative positioning) command is present in
  the file, preventing incorrect merges of relative-mode moves.
- **Collinear merge z-consistency**: Moves with inconsistent z-presence (mixing
  explicit `Z` values with absent `Z`) can no longer form a collinear run,
  preventing use of the wrong z=0.0 default in the collinearity check.
- **I003 false-positive reduction**: Temperature tower detection now requires at
  least 4 distinct temperature steps and a minimum 10 °C span before emitting
  `I003`, eliminating false positives on normal multi-material prints. Both
  `I003` messages now append "— verify this is intentional".
- **M73 accuracy**: `--insert-progress` now uses per-layer timing data to
  compute accurate `P` (percent) and `R` (remaining minutes) values instead of
  a linear layer-count approximation.

### Added

- `--no-travel-merge`: opt out of Rule 7 (same-axis travel merging) without
  disabling optimization entirely.
- `--no-feedrate-strip`: opt out of Rule 8 (redundant feedrate elimination).
- `--trust-existing-m73`: when used with `--insert-progress`, preserves
  slicer-computed M73 values and only inserts at boundaries that lack one.

## [0.2.0] - 2026-03-31

### Added

- **Rule 6 — Collinear move merging** (`--merge-collinear`, opt-in): three or more
  consecutive G1 moves that lie on the same 3D line are collapsed into a single move.
  Disabled by default because collinear merging changes the number of tool-head
  pauses, which can affect surface finish on some printers.
- **M73 progress insertion** (`--insert-progress`): injects `M73 P<pct> R<min>`
  progress markers at each layer boundary so firmware progress bars stay accurate
  after optimization.
- **Minimum layer time advisory** (`--min-layer-time <SECONDS>`): emits a `W003`
  diagnostic for any layer whose estimated print time falls below the configured
  threshold. No G-Code is modified.
- **Temperature tower detection** (`I003`): the analyzer now recognises structured
  temperature-tower sequences and emits an `I003` info diagnostic, preventing
  false-positive warnings on intentional multi-temperature profiles.

### Changed

- **Rule 7 — Consecutive same-axis travel merging** is now **always applied** when
  running `--optimize`. Redundant intermediate G0 moves on the same axis (e.g. two
  consecutive `G0 X…` commands where only the second matters) are removed
  automatically. Use `--no-travel-merge` to opt out.
- **Rule 8 — Redundant feedrate elimination** is now **always applied** when
  running `--optimize`. `F` parameters that repeat the active modal feedrate are
  stripped. Use `--no-feedrate-strip` to opt out.
- Version bumped from `0.1.0` → `0.2.0`.

### Migration notes

If your post-processing pipeline relies on exact byte-for-byte output from `0.1.0`,
the automatic application of Rules 7 and 8 will change the output. The generated
G-Code is functionally equivalent — no motion or temperatures are altered — but
travel segments and feedrate tokens will be fewer. The regression guard re-analyses
after every optimization pass and aborts if any new errors are introduced.

## [0.1.0] - 2025-01-01

### Added

- Initial release with validation (E001–E003, W001–W002, I001), optimization
  Rules 1–5, print statistics, JSON/text report formats, and OrcaSlicer
  `;LAYER_CHANGE` awareness.

[Unreleased]: https://github.com/twixinator/GCode-Sentinel/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/twixinator/GCode-Sentinel/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/twixinator/GCode-Sentinel/releases/tag/v0.1.0
