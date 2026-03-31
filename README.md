# GCode-Sentinel

High-performance G-Code validator and optimizer for 3D printing.

GCode-Sentinel parses G-Code files, simulates print-head motion to detect errors before they reach your printer, and removes redundant commands that slicers leave behind. It reads files via memory-mapped I/O, runs a zero-copy parser, and produces structured diagnostics with stable machine-readable codes.

## Features

- **Validation** -- detects out-of-bounds moves, negative coordinates, and abnormal retractions before you print
- **Optimization** -- removes empty moves, duplicate mode switches, redundant fan/temperature commands, and zero-delta moves
- **Print statistics** -- reports layer count, total distance, filament usage, estimated time, and bounding box
- **Regression guard** -- re-analyzes after optimization and aborts if new errors were introduced
- **Dual output formats** -- human-readable text summary or machine-readable JSON
- **OrcaSlicer aware** -- recognizes `;LAYER_CHANGE` comments for accurate layer counting; handles packed parameters without spaces
- **Lossless round-tripping** -- unrecognized commands (Klipper macros, thumbnail data, arbitrary G/M-codes) pass through unchanged

## Installation

### From source

Requires Rust 1.75 or later.

```sh
git clone https://github.com/twixinator/GCode-Sentinel.git
cd GCode-Sentinel
cargo build --release
```

The binary is at `target/release/gcode-sentinel`.

### With cargo install

```sh
cargo install --git https://github.com/twixinator/GCode-Sentinel.git
```

## Usage

```
gcode-sentinel [OPTIONS] <INPUT>
```

### Validate a file (check-only, no output written)

```sh
gcode-sentinel model.gcode --check-only
```

Exits with a non-zero status if errors are found. Suitable as a CI gate or pre-print check.

### Validate with machine limits

```sh
gcode-sentinel model.gcode --check-only --max-x 220 --max-y 220 --max-z 250
```

Enables out-of-bounds detection for each axis specified.

### Optimize and write to a new file

```sh
gcode-sentinel model.gcode --output model_optimized.gcode
```

Removes redundant commands and writes the cleaned G-Code to the output file.

### Optimize and write to stdout

```sh
gcode-sentinel model.gcode > model_optimized.gcode
```

When `--output` is omitted, optimized G-Code is written to stdout.

### JSON report to stdout (implies check-only)

```sh
gcode-sentinel model.gcode --report-format json
```

Prints a JSON report containing diagnostics, print statistics, and optimization changes. No G-Code output is written.

> **Schema stability (0.x):** The JSON report schema (`AnalysisReport`, `OptimizationChange`) is not yet stable. Fields may be added or removed in any 0.x release. Pin to a specific version if you depend on the schema in automation.

### Save report to a file while also writing optimized G-Code

```sh
gcode-sentinel model.gcode --output optimized.gcode --report-file report.json --report-format json
```

### Verbose logging

```sh
gcode-sentinel model.gcode --verbose
```

Prints debug-level tracing output to stderr. Controlled by `--verbose` or the `RUST_LOG` environment variable.

### CLI reference

| Flag | Description |
|------|-------------|
| `<INPUT>` | Path to the input G-Code file (required) |
| `-o, --output <PATH>` | Output file path; stdout if omitted |
| `--check-only` | Validate only, do not write optimized output |
| `--max-x <MM>` | Machine X-axis travel limit (mm) |
| `--max-y <MM>` | Machine Y-axis travel limit (mm) |
| `--max-z <MM>` | Machine Z-axis travel limit (mm) |
| `--report-file <PATH>` | Write the analysis report to this file |
| `--report-format <FMT>` | Report format: `text` (default) or `json` |
| `-c, --config <PATH>` | Path to a TOML configuration file |
| `-v, --verbose` | Enable debug output |
| `--merge-collinear` | (Opt-in) Merge consecutive collinear G1 moves into one |
| `--insert-progress` | Strip existing M73 markers and re-insert recalculated ones at each layer boundary |
| `--trust-existing-m73` | When used with `--insert-progress`, preserve slicer M73 values instead of stripping them |
| `--min-layer-time <SECONDS>` | Emit a `W003` warning for any layer below this estimated print time |
| `--no-travel-merge` | Disable Rule 7 — keep all intermediate same-axis travel moves |
| `--no-feedrate-strip` | Disable Rule 8 — preserve all `F` parameters even when redundant |

## Example output

### Text report (stderr)

```
═══ GCode-Sentinel Report ═══
Layers    : 142
Moves     : 284710
Distance  : 98234.5 mm
Filament  : 4521.3 mm
Est. time : 87 min
Bbox min  : (0.4, 0.4, 0.2)
Bbox max  : (219.6, 219.6, 28.4)

Diagnostics (3):
  [warning] line 4012: W001 — move to negative coordinate (-0.5, 10, 0.2)
  [error] line 8844: E001 — X 250.000 exceeds machine limit of 220.000 mm
  [info] line 102: W002 — extruder retraction: E delta -0.800 mm

Optimized (5 changes):
  line 14: empty move with no parameters
  line 87: duplicate mode switch (G90/G91 already active)
  line 1204: duplicate fan command (same setting already active)
  line 3001: zero-delta move (no net displacement)
  line 5400: duplicate temperature command (already set)
```

## Architecture

The processing pipeline is a linear chain of four stages:

```
input.gcode
    |
    v
 [Parser]       Zero-copy, line-oriented scanner. Produces a Vec<Spanned<GCodeCommand>>.
    |            Handles G0, G1, G90, G91, G92, all M-codes, comments, and unknown lines.
    v
 [Analyzer]     Virtual print-head simulation. Walks the AST once to produce
    |            diagnostics (errors, warnings, info) and print statistics
    v            (layers, distance, filament, time, bounding box).
 [Optimizer]    Conservative single-pass redundancy removal. Only removes commands
    |            that are mathematically certain to have no observable effect.
    v            Re-analysis after optimization detects regressions.
 [Emitter]      Serializes the (possibly reduced) AST back to G-Code text.
    |
    v
 output.gcode
```

## Diagnostic codes

| Code | Severity | Description |
|------|----------|-------------|
| `E001` | Error | X coordinate exceeds machine limit |
| `E002` | Error | Y coordinate exceeds machine limit |
| `E003` | Error | Z coordinate exceeds machine limit |
| `W001` | Warning | Move to negative coordinate |
| `W002` | Warning/Info | Extruder retraction detected (Warning if >= 2mm, Info if < 2mm) |
| `I001` | Info | Layer change detected via Z increase |

## Optimization rules

| Rule | Description |
|------|-------------|
| 1 | Remove empty moves (G0/G1 with no parameters) |
| 2 | Remove duplicate consecutive mode switches (G90 after G90, G91 after G91) |
| 3 | Remove duplicate consecutive fan commands (M106/M107 with identical parameters) |
| 4 | Remove zero-delta moves (absolute move to current position with no feedrate change) |
| 5 | Remove duplicate consecutive temperature commands (M104/M109/M140/M190) |

## Testing

```sh
cargo test            # unit + integration tests
cargo test --release  # release-mode tests
```

The test suite includes unit tests for every pipeline stage, integration tests against real OrcaSlicer G-Code fixtures, and round-trip fidelity checks ensuring parse-emit-parse stability.

## CI

GitHub Actions runs on every push and pull request against `main`:

- `cargo fmt --check`
- `cargo clippy -- -D warnings`
- `cargo test` (debug and release)
- Matrix: stable + nightly Rust, Ubuntu + Windows

## Contributing

1. Fork the repository and create a feature branch (`feature/your-feature`).
2. Write tests first (red-green-refactor). Aim for descriptive Given-When-Then test names.
3. Run `cargo fmt` and `cargo clippy -- -D warnings` before committing.
4. Use [Conventional Commits](https://www.conventionalcommits.org/) (`feat:`, `fix:`, `refactor:`, etc.).
5. Open a pull request against `main`.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
