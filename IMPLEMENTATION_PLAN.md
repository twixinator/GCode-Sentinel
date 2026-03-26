# Architecture Review: GCode-Sentinel

**Reviewer:** Software Architect (Automation & Safety-Critical Systems)
**Date:** 2026-03-26
**Status:** Greenfield -- no existing code. Review based on proposed module structure and constraints.

---

## 1. Module Boundaries

### What is sound

The six-module split (cli, models, parser, analyzer, optimizer, lib) follows a clean layered architecture. Each module has a single, well-defined responsibility. The library/binary separation (`lib.rs` re-exports, `main.rs` is the thin binary) is the correct pattern for testability.

### Concerns

**1.1 `models.rs` will become a gravity well.**

Every module depends on `models.rs`. That is expected for core types, but the current grouping -- `GCodeCommand`, `Point3D`, `MachineLimits` -- mixes three distinct concerns:

- **AST types** (`GCodeCommand` and its variants) -- consumed by parser, analyzer, optimizer
- **Geometry types** (`Point3D`, potentially `BoundingBox`, `Vector3D`) -- consumed by analyzer, optimizer
- **Configuration types** (`MachineLimits`) -- consumed by analyzer and CLI

As the project grows, a single `models.rs` becomes the file everyone edits and everyone conflicts on.

**Recommendation:** Split into a `models/` directory early:
- `models/ast.rs` -- G-Code AST node types
- `models/geometry.rs` -- `Point3D`, `BoundingBox`, movement primitives
- `models/config.rs` -- `MachineLimits`, machine profiles
- `models/mod.rs` -- re-exports

This costs nothing now and prevents painful refactoring later when the AST alone grows to 20+ variants (G0, G1, G28, G90/G91, M104, M106, M109, comments, unknown commands, etc.).

**1.2 Missing: `emitter.rs` / `writer.rs`**

The pipeline is Input -> Parse -> Analyze -> Optimize -> **???** -> Output. There is no module responsible for serializing the optimized AST back to G-Code text. This is not trivial -- G-Code formatting has conventions (decimal precision, line endings, comment preservation, checksum lines for some firmware).

**Recommendation:** Add `emitter.rs` with responsibility: AST -> G-Code text output. This keeps serialization concerns out of `optimizer.rs` and makes the pipeline symmetric: `parser` (text->AST) and `emitter` (AST->text).

**1.3 Missing: `report.rs` or `diagnostics.rs`**

The analyzer detects out-of-bounds moves and mechanical conflicts. Where do those diagnostics go? If they are just printed to stderr, you lose the ability to:
- Produce structured JSON reports (CI integration)
- Collect multiple warnings before aborting
- Let the optimizer act on analyzer findings

**Recommendation:** Add `diagnostics.rs` defining a `Diagnostic` type with severity, source location (line number, byte offset), message, and optional fix suggestion. The analyzer produces `Vec<Diagnostic>`, the binary decides how to render them.

---

## 2. Data Flow

### Proposed pipeline: File -> Parser -> AST -> Analyzer -> Optimizer -> Output

**2.1 The pipeline is fundamentally sound but should be two-pass, not single-pass.**

- **Pass 1 (streaming):** Parse + Analyze. Walk the file linearly, build the virtual print head state, emit diagnostics. For files that only need validation (no optimization), this pass is sufficient and can operate in constant memory.
- **Pass 2 (AST-based):** Optimize + Emit. Only triggered when optimization is requested. Requires the full AST in memory (or a chunk-based approach for GB files -- see Section 6).

This distinction matters because validation-only mode should not require holding the full AST. A user running `gcode-sentinel check bigfile.gcode` should get results in seconds with minimal memory.

**Recommendation:** Design the parser to support both modes:
- `parse_streaming(input) -> impl Iterator<Item = Result<GCodeCommand>>` for validation
- `parse_all(input) -> Result<Vec<GCodeCommand>>` for optimization

Both can be zero-copy over the same mmap'd buffer.

**2.2 Analyzer should not mutate the AST.**

Keep the analyzer as a pure read-only pass that produces diagnostics. If the optimizer needs information the analyzer computed (e.g., "this travel move crosses a printed region"), pass that as a separate analysis result struct, not by annotating the AST.

**Rationale:** Mutable AST nodes force you into `&mut` borrows which conflict with zero-copy `&'a str` references. Keep the AST immutable after parsing.

---

## 3. Public API Boundaries

Concrete recommendations for each module's public surface:

### `parser`
```
pub fn parse_line(input: &str) -> Result<GCodeCommand, ParseError>
pub fn parse_streaming(input: &str) -> impl Iterator<Item = Result<GCodeCommand, ParseError>>
pub fn parse_all(input: &str) -> Result<Vec<GCodeCommand>, ParseError>
```
- `ParseError` via thiserror, includes line number and byte offset
- All functions take `&'a str` and return types borrowing from that lifetime

### `analyzer`
```
pub struct AnalysisResult { pub diagnostics: Vec<Diagnostic>, pub stats: PrintStats }
pub fn analyze(commands: impl Iterator<Item = &GCodeCommand>, limits: &MachineLimits) -> AnalysisResult
```
- Takes an iterator, not a Vec -- works with both streaming and full-AST modes
- `PrintStats`: estimated print time, filament usage, bounding box, layer count
- Pure function, no side effects

### `optimizer`
```
pub struct OptimizationResult { pub commands: Vec<GCodeCommand>, pub changes: Vec<OptimizationChange> }
pub fn optimize(commands: Vec<GCodeCommand>, limits: &MachineLimits, config: &OptConfig) -> OptimizationResult
```
- Takes ownership of the command Vec (it will reorder/remove elements)
- Returns both the optimized commands and a changelog for transparency
- `OptConfig` controls which optimizations are enabled (users must opt in to aggressive changes)

### `emitter`
```
pub fn emit<W: Write>(commands: &[GCodeCommand], writer: &mut W, config: &EmitConfig) -> Result<(), EmitError>
```
- Generic over `Write` for testability (write to String, file, stdout)
- `EmitConfig`: decimal precision, line ending style, comment preservation

### `diagnostics`
```
pub struct Diagnostic { pub severity: Severity, pub line: usize, pub message: String, pub code: &'static str }
pub enum Severity { Error, Warning, Info }
```
- `code` is a stable identifier like `"E001"` for machine-readable output

---

## 4. Zero-Copy Lifetime Design

This is the hardest architectural decision in the project. Here is the analysis:

### 4.1 The lifetime chain

```
mmap'd file (&'a [u8])
  -> validated as UTF-8 (&'a str)
    -> parser produces GCodeCommand<'a> borrowing substrings
      -> analyzer reads &GCodeCommand<'a>
      -> optimizer takes Vec<GCodeCommand<'a>>
        -> emitter reads &[GCodeCommand<'a>]
```

The mmap handle must outlive everything. In practice this means:

```
fn run() {
    let mmap = open_mmap(path);          // lives here
    let input: &str = validate_utf8(&mmap);
    let commands = parse_all(input);      // borrows from input
    let analysis = analyze(&commands);    // borrows from commands
    let optimized = optimize(commands);   // takes ownership, still borrows from input
    emit(&optimized, &mut output);
}
```

This is clean as long as all lifetimes are anchored to the mmap in a single function scope or a struct that holds both the mmap and the parsed data together.

### 4.2 The ownership trap

**Problem:** The optimizer must reorder and remove commands. If `GCodeCommand<'a>` contains `&'a str` slices pointing into the mmap'd file, the optimizer can reorder references freely -- but it cannot **create new commands** (e.g., inserting a retraction move) because there is no backing string to borrow from.

**Options:**

| Approach | Pro | Con |
|---|---|---|
| A) `GCodeCommand<'a>` is fully zero-copy | Fastest parsing, lowest memory | Optimizer cannot synthesize new commands |
| B) `GCodeCommand` owns all strings (`String`) | Optimizer is unconstrained | Defeats zero-copy, doubles memory on large files |
| C) Hybrid: `Cow<'a, str>` for string fields | Zero-copy by default, owned when needed | Slightly more complex API, `Cow` has a branch on every access |
| D) AST stores parsed numeric values, not strings | No string references needed in optimized AST | Parser does more work upfront, but numeric representation is what analyzer/optimizer actually need |

**Recommendation: Option D.**

The key insight is that the analyzer and optimizer do not care about the original text `"G1 X10.5 Y20.3 F1200"`. They care about the parsed values: command type G1, X=10.5, Y=20.3, F=1200. Store those as enums and `f64`/`f32` values. The only string data worth preserving is comments, and those can use `Cow<'a, str>`.

```
enum GCodeCommand<'a> {
    LinearMove { x: Option<f64>, y: Option<f64>, z: Option<f64>, e: Option<f64>, f: Option<f64> },
    RapidMove { x: Option<f64>, y: Option<f64>, z: Option<f64>, f: Option<f64> },
    SetTemperature { tool: u8, temp: f64, wait: bool },
    Comment(Cow<'a, str>),
    Unknown(Cow<'a, str>),  // preserve unrecognized lines verbatim
    // ...
}
```

This gives you:
- Zero-copy where it matters (comments, unknown lines preserved without allocation)
- Numeric data parsed once, used everywhere
- Optimizer can freely create synthetic commands (no lifetime issues for numeric fields)
- `f64` is 8 bytes -- same as a fat pointer, no memory penalty

### 4.3 Use `f64`, not `f32`

G-Code coordinates accumulate over thousands of moves. `f32` has ~7 decimal digits of precision. A 300mm bed with 0.001mm precision needs 6 digits before any accumulation. After summing thousands of relative moves in G91 mode, `f32` will drift. `f64` is the safe default. The memory difference is negligible compared to the strings you are already saving.

---

## 5. Error Handling Strategy

### 5.1 `thiserror` in lib + `anyhow` in binary: Correct pattern.

This is idiomatic Rust. No concerns with the general approach.

### 5.2 Specific recommendations

**Parser errors must be recoverable.** A single malformed line should not abort parsing of a 2GB file. Design for this:

- `ParseError` is a per-line error
- `parse_streaming` yields `Result<GCodeCommand, ParseError>` per line
- The caller (analyzer or binary) decides the policy: skip and warn, collect N errors then abort, or fail-fast

**Define error types per module, not one global error enum:**

- `parser::ParseError` -- line number, byte offset, expected vs found
- `analyzer::AnalysisError` -- for fatal analysis failures (not diagnostics -- those are warnings, not errors)
- `optimizer::OptimizeError` -- invariant violations during optimization
- `emitter::EmitError` -- I/O failures during output

Each wraps into a library-level `GCodeError` via `thiserror` if needed, and the binary maps to `anyhow::Error`.

### 5.3 Do not use `anyhow` in integration tests

Integration tests exercise the library API. They should assert on specific error variants, which requires `thiserror` types. Using `anyhow` in tests hides regressions where the error type changes but the test still passes because it only checks `is_err()`.

---

## 6. Scalability: GB-Scale Files

### 6.1 Memory-mapped I/O with `memmap2`: Correct choice.

The OS handles paging. A 2GB file does not require 2GB of RAM if you access it sequentially. The mmap approach is sound.

### 6.2 The real bottleneck is the AST, not the input.

Consider: a typical G-Code file has ~50 bytes per line. A 2GB file has ~40 million lines. If `GCodeCommand` is 64 bytes (realistic with the Option<f64> fields), the parsed AST is ~2.5 GB. You have doubled your memory usage.

**For validation-only mode:** Use the streaming iterator. Never allocate the full AST. Memory usage stays constant regardless of file size. This is the common case -- most users run validation more often than optimization.

**For optimization mode with GB-scale files:** You have three options:

| Approach | Complexity | Memory | Effectiveness |
|---|---|---|---|
| A) Load full AST, optimize in-place | Low | O(n) -- GB-scale | Works for files that fit in RAM |
| B) Layer-chunked processing | Medium | O(layer_size) | G-Code is naturally layered; optimize per-layer, emit, drop |
| C) Two-pass: analyze then patch | High | O(changes) | Only store the diff, apply patches in a streaming second pass |

**Recommendation: Start with A, design for B.**

Ship v1 with full-AST optimization. Most real-world G-Code files are 50-500 MB (large but manageable). Add a `--max-memory` flag that refuses to optimize files beyond a threshold and suggests `--check-only` mode.

Design the optimizer's internal API around layer boundaries from day one (`optimize_layer(commands: &mut [GCodeCommand])`) so that migrating to approach B later is a refactor, not a rewrite.

### 6.3 UTF-8 validation cost

`std::str::from_utf8` on a 2GB mmap'd buffer is not free -- it must scan the entire file. For G-Code (ASCII-only in practice), consider using `memchr` or a SIMD-accelerated ASCII check first, falling back to full UTF-8 validation only if high bytes are found. Or use `from_utf8_unchecked` behind a `--trust-ascii` flag with appropriate warnings.

Alternatively, skip whole-file UTF-8 validation entirely and validate per-line during parsing. This amortizes the cost and gives better error messages ("invalid UTF-8 on line 4231" vs "invalid UTF-8 in file").

---

## 7. Additional Concerns

### 7.1 Missing: Line number tracking

Every `GCodeCommand` should carry its source line number (and ideally byte offset). This is essential for:
- Meaningful error messages
- Diagnostic output referencing specific lines
- The emitter preserving or referencing original line numbers
- Debugging optimizer changes ("moved line 4231 before line 4228")

Add `line: u32` (supports files up to 4 billion lines) to each command or wrap as `Spanned<GCodeCommand> { inner: GCodeCommand, line: u32, offset: u64 }`.

### 7.2 Missing: Configuration / machine profile system

`MachineLimits` as a struct is fine for v1, but 3D printers vary wildly. Consider a TOML/JSON config file for machine profiles:
```
[machine]
name = "Prusa MK4"
max_x = 250.0
max_y = 210.0
max_z = 210.0
max_feedrate = 12000.0
```

Add a `--machine` CLI flag that loads a profile. Ship a few common profiles. This is a v2 concern but the architecture should not make it hard -- keeping `MachineLimits` in its own `config` module enables this cleanly.

### 7.3 `nom` vs `logos` choice

These serve different purposes:
- **`nom`**: Parser combinator library. Good for structured, recursive grammars. Overkill for G-Code which is strictly line-oriented with no nesting.
- **`logos`**: Lexer generator. Fast tokenization. Does not help with parsing the token stream into an AST.
- **Neither may be necessary.** G-Code is simple enough that a hand-written line parser (split on spaces, match first token) will be faster than either library and have zero dependencies.

**Recommendation:** Start with a hand-written parser. G-Code has no recursion, no operator precedence, no ambiguity. Each line is independent. A hand-written parser will be:
- Faster (no combinator overhead, no regex compilation)
- Easier to debug
- Zero additional dependencies
- Easier to produce precise error messages with line/column info

If profiling later shows the parser is a bottleneck, `nom` is the appropriate escalation -- but I would be surprised. The I/O and AST allocation will dominate.

### 7.4 Consider `nom` only if you plan to support non-standard G-Code dialects

Some slicers emit non-standard extensions (PrusaSlicer thumbnails in comments, Marlin-specific M commands, Klipper macros). If supporting multiple dialects is a goal, `nom`'s composability helps. But define the scope first.

### 7.5 Testing strategy outline

Since this is a library, the testing surface is clean:

- **Parser:** Roundtrip tests (parse -> emit -> parse, assert equality). Fuzz testing with `cargo-fuzz` on the parser is high-value -- G-Code files come from untrusted slicers.
- **Analyzer:** Property-based tests with `proptest` -- generate random move sequences, verify the virtual print head never contradicts itself.
- **Optimizer:** Golden-file tests -- known input G-Code, expected output G-Code, diff on mismatch. This catches regressions in optimization logic immediately.
- **Integration:** End-to-end CLI tests using `assert_cmd` and `predicates` crates.

### 7.6 Safety note

This tool does not directly control hardware (it processes files offline), so the full safety architecture (state machines, heartbeats, fail-safe states) does not apply. However:

**The optimizer modifies G-Code that will control physical hardware.** A bug in the optimizer that reorders a temperature command before a move, or removes a retraction, can cause:
- Nozzle crashes (Z move removed or reordered)
- Thermal runaway (heater command removed)
- Print failure (extrusion commands reordered)

**Mitigation requirements:**
1. The optimizer must be conservative by default. Only enable optimizations the user explicitly requests.
2. Every optimization must have a `--dry-run` mode that shows what would change without modifying the file.
3. The analyzer should run **after** optimization as well, validating that the optimized output still passes all safety checks. If post-optimization analysis finds new violations, abort and report.
4. Golden-file regression tests for the optimizer are mandatory, not optional.

---

## 8. Revised Module Structure

| File/Dir | Responsibility |
|---|---|
| `main.rs` | Entry point: CLI init, tracing init, orchestrates pipeline |
| `cli.rs` | CLI arguments via clap derive |
| `models/mod.rs` | Re-exports |
| `models/ast.rs` | `GCodeCommand`, `Spanned<T>` wrapper with line/offset |
| `models/geometry.rs` | `Point3D`, `BoundingBox`, movement primitives |
| `models/config.rs` | `MachineLimits`, `OptConfig`, `EmitConfig` |
| `parser.rs` | Hand-written line parser, streaming + batch modes |
| `analyzer.rs` | Read-only validation, produces `AnalysisResult` with `Vec<Diagnostic>` |
| `optimizer.rs` | AST transformations, layer-aware internal API |
| `emitter.rs` | AST -> G-Code text serialization |
| `diagnostics.rs` | `Diagnostic`, `Severity`, formatting for terminal/JSON |
| `lib.rs` | Re-exports, `#![warn(clippy::pedantic)]` |

---

## 9. Summary of Recommendations

| # | Recommendation | Priority | Rationale |
|---|---|---|---|
| 1 | Split `models.rs` into `models/` directory | High | Prevents merge conflicts and god-file as AST grows |
| 2 | Add `emitter.rs` | High | Pipeline has no output stage |
| 3 | Add `diagnostics.rs` | High | Structured error reporting for CI and user experience |
| 4 | Use parsed numeric values in AST, not string refs (Option D) | High | Solves the optimizer ownership problem cleanly |
| 5 | Use `f64` for coordinates | High | Prevents precision drift in relative mode |
| 6 | Add line number tracking to every command | High | Essential for usable error messages |
| 7 | Support streaming parse for validation-only mode | High | Constant-memory validation of GB files |
| 8 | Start with hand-written parser, not `nom`/`logos` | Medium | G-Code is too simple to justify parser combinator overhead |
| 9 | Design optimizer API around layer boundaries | Medium | Enables future chunk-based processing without rewrite |
| 10 | Run analyzer post-optimization as safety check | Medium | Catches optimizer bugs before they reach hardware |
| 11 | Per-line UTF-8 validation instead of whole-file | Medium | Better error messages, amortized cost |
| 12 | Add machine profile config files (TOML) | Low | v2 feature, but keep architecture compatible |
| 13 | Per-module error types, not a single global enum | Low | Cleaner API, better test assertions |

---

## 10. Open Questions for the Author

1. **Dialect scope:** Which slicers must be supported? (PrusaSlicer, Cura, OrcaSlicer, Simplify3D all have dialect quirks.) This affects parser complexity significantly.
2. **Optimization scope for v1:** Which specific optimizations are planned? Travel path reordering? Redundant command elimination? Retraction optimization? The answer affects whether the layer-chunked API is needed immediately.
3. **Output format:** Does the tool only produce optimized G-Code files, or also reports (JSON, terminal summary, diff view)?
4. **CI integration:** Is there a plan to use this as a CI check (e.g., validate G-Code in a slicer profile repo)? If so, the exit code semantics and JSON output format should be designed upfront.
