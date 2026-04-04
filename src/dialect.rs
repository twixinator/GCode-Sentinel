//! Slicer dialect detection and metadata extraction.
//!
//! This module identifies which slicer generated a G-Code file by scanning
//! discriminating comment patterns in the first [`SCAN_LIMIT`] commands, and
//! then extracts slicer-embedded metadata (nozzle diameter, layer height,
//! filament type, temperatures, estimated print time) from header or footer
//! comments.
//!
//! # Dialect detection
//!
//! Each slicer embeds a characteristic marker comment near the top of the file.
//! [`detect_dialect`] scans the first [`SCAN_LIMIT`] commands for these markers
//! and returns the first match.  If no marker is found the function returns
//! `None` (unknown dialect).
//!
//! # Metadata extraction
//!
//! [`extract_metadata`] performs a **full-file** scan.  PrusaSlicer and
//! SuperSlicer embed metadata in end-of-file comments; Cura embeds it in a
//! header block using `;KEY: value` syntax.  The function returns a
//! [`SlicerMetadata`] whose fields are all `Option` — only values that are
//! actually present in the file are populated.
//!
//! # Layer-change signal
//!
//! Cura emits `;LAYER:N` comments at every layer boundary.  The [`is_cura_layer_change`]
//! helper lets other modules (e.g. the analyser) recognise these comments
//! without a hard dependency on this module's detection logic.

// Slicer brand names (OrcaSlicer, PrusaSlicer, SuperSlicer, Simplify3D,
// IdeaMaker) are proper nouns, not code identifiers.  Forcing backtick
// wrapping in prose makes the documentation harder to read without adding
// semantic value, so we suppress the lint at module scope.
#![allow(clippy::doc_markdown)]

use crate::models::{GCodeCommand, Spanned};

// ─────────────────────────────────────────────────────────────────────────────
// Scan limit
// ─────────────────────────────────────────────────────────────────────────────

/// Number of commands examined by [`detect_dialect`].
///
/// Dialect markers appear in the header, so 100 commands is far more than
/// enough for any known slicer while keeping detection O(1) in practice.
pub const SCAN_LIMIT: usize = 100;

// ─────────────────────────────────────────────────────────────────────────────
// Discriminating comment patterns (named constants — no magic strings)
// ─────────────────────────────────────────────────────────────────────────────

/// OrcaSlicer header marker (exact prefix).
const ORCA_MARKER: &str = "OrcaSlicer";
/// PrusaSlicer header marker (exact prefix).
const PRUSA_MARKER: &str = "PrusaSlicer";
/// SuperSlicer header marker (exact prefix).
const SUPER_MARKER: &str = "SuperSlicer";
/// Cura flavor header marker (exact prefix in Cura headers).
const CURA_FLAVOR_MARKER: &str = "FLAVOR:";
/// Simplify3D header marker.
const SIMPLIFY3D_MARKER: &str = "Simplify3D";
/// IdeaMaker header marker.
const IDEAMAKER_MARKER: &str = "ideaMaker";

// ─────────────────────────────────────────────────────────────────────────────
// Cura metadata keys
// ─────────────────────────────────────────────────────────────────────────────

/// Cura header key for layer height.
const CURA_KEY_LAYER_HEIGHT: &str = "Layer height:";
/// Cura header key for nozzle diameter.
const CURA_KEY_NOZZLE_DIAMETER: &str = "Nozzle diameter:";
/// Cura header key for material type (filament).
const CURA_KEY_MATERIAL: &str = "Material:";
/// Cura header key for print temperature.
const CURA_KEY_PRINT_TEMP: &str = "Extruder 1 start temperature:";
/// Cura header key for bed temperature.
const CURA_KEY_BED_TEMP: &str = "Bed temperature:";
/// Cura header key for estimated print time.
const CURA_KEY_TIME: &str = "Print time (s):";

// ─────────────────────────────────────────────────────────────────────────────
// PrusaSlicer / SuperSlicer metadata keys (end-of-file comments)
// ─────────────────────────────────────────────────────────────────────────────

/// PrusaSlicer/SuperSlicer comment key for layer height.
const PRUSA_KEY_LAYER_HEIGHT: &str = "layer_height";
/// PrusaSlicer/SuperSlicer comment key for nozzle diameter.
const PRUSA_KEY_NOZZLE_DIAMETER: &str = "nozzle_diameter";
/// PrusaSlicer/SuperSlicer comment key for filament type.
const PRUSA_KEY_FILAMENT_TYPE: &str = "filament_type";
/// PrusaSlicer/SuperSlicer comment key for first-layer hotend temperature.
const PRUSA_KEY_FIRST_LAYER_TEMP: &str = "first_layer_temperature";
/// PrusaSlicer/SuperSlicer comment key for bed temperature.
const PRUSA_KEY_BED_TEMP: &str = "first_layer_bed_temperature";
/// PrusaSlicer/SuperSlicer comment key for estimated print time in seconds.
const PRUSA_KEY_ESTIMATED_TIME: &str = "estimated printing time (normal mode)";

// ─────────────────────────────────────────────────────────────────────────────
// Cura layer-change signal
// ─────────────────────────────────────────────────────────────────────────────

/// Prefix that Cura uses for its layer-change comments (`;LAYER:N`).
const CURA_LAYER_PREFIX: &str = "LAYER:";

// ─────────────────────────────────────────────────────────────────────────────
// Public types
// ─────────────────────────────────────────────────────────────────────────────

/// Identified slicer that generated the G-Code file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Dialect {
    /// OrcaSlicer — identifies itself with `;OrcaSlicer` header.
    OrcaSlicer,
    /// PrusaSlicer — identifies itself with `;PrusaSlicer` header.
    PrusaSlicer,
    /// SuperSlicer — identifies itself with `;SuperSlicer` header.
    SuperSlicer,
    /// Ultimaker Cura — identifies itself with `;FLAVOR:` header.
    Cura,
    /// Simplify3D — identifies itself with `;Simplify3D` header.
    Simplify3D,
    /// Raise3D IdeaMaker — identifies itself with `;ideaMaker` header.
    IdeaMaker,
}

/// Slicer-embedded print metadata extracted from comments.
///
/// All fields are `Option` because the specific values may or may not be
/// present depending on the slicer version, configuration, and which fields
/// are enabled in the slicer's output.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize)]
pub struct SlicerMetadata {
    /// Nozzle (hotend) diameter in millimetres, e.g. `0.4`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nozzle_diameter: Option<f64>,

    /// Layer height in millimetres, e.g. `0.2`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer_height: Option<f64>,

    /// Filament type string, e.g. `"PLA"`, `"PETG"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filament_type: Option<String>,

    /// First-layer / print hotend temperature in degrees Celsius.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub print_temperature: Option<f64>,

    /// Bed temperature in degrees Celsius.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bed_temperature: Option<f64>,

    /// Slicer-estimated total print time in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_time_seconds: Option<f64>,
}

impl SlicerMetadata {
    /// Returns `true` if every field is `None` (nothing was extracted).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nozzle_diameter.is_none()
            && self.layer_height.is_none()
            && self.filament_type.is_none()
            && self.print_temperature.is_none()
            && self.bed_temperature.is_none()
            && self.estimated_time_seconds.is_none()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Public API
// ─────────────────────────────────────────────────────────────────────────────

/// Detect the slicer dialect by scanning the first [`SCAN_LIMIT`] commands.
///
/// Returns `Some(dialect)` when a discriminating header comment is found within
/// the first [`SCAN_LIMIT`] commands, or `None` if no known marker is present.
///
/// Detection is order-sensitive: if a file somehow contains markers for
/// multiple dialects (which should not happen in practice), the one whose
/// marker appears first wins.
///
/// # Example
///
/// ```rust
/// use std::borrow::Cow;
/// use gcode_sentinel::dialect::detect_dialect;
/// use gcode_sentinel::models::{GCodeCommand, Spanned};
///
/// let commands = vec![
///     Spanned {
///         inner: GCodeCommand::Comment {
///             text: Cow::Borrowed("Generated by OrcaSlicer 2.0.0"),
///         },
///         line: 1,
///         byte_offset: 0,
///     },
/// ];
/// let dialect = detect_dialect(&commands);
/// assert!(dialect.is_some());
/// ```
#[must_use]
pub fn detect_dialect(commands: &[Spanned<GCodeCommand<'_>>]) -> Option<Dialect> {
    for spanned in commands.iter().take(SCAN_LIMIT) {
        if let Some(dialect) = match_dialect_comment(spanned) {
            return Some(dialect);
        }
    }
    None
}

/// Extract slicer-embedded metadata from the full command list.
///
/// Scans all commands (not just the first [`SCAN_LIMIT`]) because metadata
/// comments are distributed across the file:
/// - Cura embeds metadata in a **header** block near the top.
/// - PrusaSlicer and SuperSlicer embed metadata in **footer** comments at the
///   end of the file.
///
/// Fields not found in the file remain `None`.  The returned [`SlicerMetadata`]
/// is always safe to use regardless of which fields are populated.
///
/// # Example
///
/// ```rust
/// use std::borrow::Cow;
/// use gcode_sentinel::dialect::extract_metadata;
/// use gcode_sentinel::models::{GCodeCommand, Spanned};
///
/// let commands = vec![
///     Spanned {
///         inner: GCodeCommand::Comment {
///             text: Cow::Borrowed("nozzle_diameter = 0.4"),
///         },
///         line: 1,
///         byte_offset: 0,
///     },
/// ];
/// let meta = extract_metadata(&commands);
/// assert_eq!(meta.nozzle_diameter, Some(0.4));
/// ```
#[must_use]
pub fn extract_metadata(commands: &[Spanned<GCodeCommand<'_>>]) -> SlicerMetadata {
    let mut meta = SlicerMetadata::default();

    for spanned in commands {
        if let GCodeCommand::Comment { text } = &spanned.inner {
            apply_comment_to_metadata(text.as_ref(), &mut meta);
        }
    }

    meta
}

/// Returns `true` when `text` is a Cura layer-change comment (`LAYER:N`).
///
/// Cura emits `;LAYER:0`, `;LAYER:1`, etc. at every layer boundary.  The
/// comment text arrives here stripped of the leading `;`, so we match
/// against the `LAYER:` prefix only.
///
/// # Example
///
/// ```rust
/// use gcode_sentinel::dialect::is_cura_layer_change;
///
/// assert!(is_cura_layer_change("LAYER:0"));
/// assert!(is_cura_layer_change("LAYER:42"));
/// assert!(!is_cura_layer_change("LAYER_CHANGE"));
/// assert!(!is_cura_layer_change("layer:0"));
/// ```
#[must_use]
pub fn is_cura_layer_change(comment_text: &str) -> bool {
    comment_text.starts_with(CURA_LAYER_PREFIX)
}

// ─────────────────────────────────────────────────────────────────────────────
// Internal helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Test a single command for a dialect marker.
///
/// Only `Comment` and `Unknown` (raw) lines are tested because all known
/// slicer markers appear in comment form.  `Unknown` lines handle the rare
/// case where a slicer comment does not start with `;` and is not parsed
/// as a `Comment` variant (e.g. blank-or-whitespace lines prepended before
/// the first `;`).
fn match_dialect_comment(spanned: &Spanned<GCodeCommand<'_>>) -> Option<Dialect> {
    let text = match &spanned.inner {
        GCodeCommand::Comment { text } => text.as_ref(),
        GCodeCommand::Unknown { raw } => raw.as_ref(),
        _ => return None,
    };
    classify_text(text)
}

/// Map a comment string to a [`Dialect`] variant by checking known markers.
///
/// Order matters: SuperSlicer must be checked before PrusaSlicer because
/// SuperSlicer's marker contains "SuperSlicer" while PrusaSlicer's contains
/// "PrusaSlicer".  A SuperSlicer file will contain both markers if it inherits
/// PrusaSlicer branding, but in practice only one marker appears at the top.
fn classify_text(text: &str) -> Option<Dialect> {
    if text.contains(ORCA_MARKER) {
        Some(Dialect::OrcaSlicer)
    } else if text.contains(SUPER_MARKER) {
        Some(Dialect::SuperSlicer)
    } else if text.contains(PRUSA_MARKER) {
        Some(Dialect::PrusaSlicer)
    } else if text.contains(CURA_FLAVOR_MARKER) {
        Some(Dialect::Cura)
    } else if text.contains(SIMPLIFY3D_MARKER) {
        Some(Dialect::Simplify3D)
    } else if text.contains(IDEAMAKER_MARKER) {
        Some(Dialect::IdeaMaker)
    } else {
        None
    }
}

/// Apply a single comment's content to the metadata accumulator.
///
/// Handles both Cura-style (`KEY: value`) and PrusaSlicer-style (`key = value`)
/// comment formats.  Unknown comment content is silently ignored.
fn apply_comment_to_metadata(text: &str, meta: &mut SlicerMetadata) {
    // Cura format: `;Key: value` — colon separator, space after colon.
    try_cura_metadata(text, meta);
    // PrusaSlicer / SuperSlicer format: `; key = value` — equals separator.
    try_prusa_metadata(text, meta);
}

/// Parse Cura header-style metadata from a comment.
///
/// Cura uses `;Key: value` format.  The comment text arrives here with the
/// leading `;` stripped, so we match on the key prefix directly.
fn try_cura_metadata(text: &str, meta: &mut SlicerMetadata) {
    if let Some(val) = strip_cura_key(text, CURA_KEY_LAYER_HEIGHT) {
        meta.layer_height = parse_f64(val);
    } else if let Some(val) = strip_cura_key(text, CURA_KEY_NOZZLE_DIAMETER) {
        meta.nozzle_diameter = parse_f64(val);
    } else if let Some(val) = strip_cura_key(text, CURA_KEY_MATERIAL) {
        meta.filament_type = Some(val.trim().to_owned());
    } else if let Some(val) = strip_cura_key(text, CURA_KEY_PRINT_TEMP) {
        meta.print_temperature = parse_f64(val);
    } else if let Some(val) = strip_cura_key(text, CURA_KEY_BED_TEMP) {
        meta.bed_temperature = parse_f64(val);
    } else if let Some(val) = strip_cura_key(text, CURA_KEY_TIME) {
        meta.estimated_time_seconds = parse_f64(val);
    }
}

/// Strip a Cura-style key prefix and return the value portion, or `None`.
fn strip_cura_key<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    // Cura comments may or may not include a leading space after ';'.
    let text = text.trim_start();
    text.strip_prefix(key)
}

/// Parse PrusaSlicer/SuperSlicer end-of-file comment metadata.
///
/// These comments use the format `; key = value` where the leading `; ` is
/// stripped before this function is called (the comment text field contains
/// everything after the `;`).  We look for ` key = value` or `key = value`.
fn try_prusa_metadata(text: &str, meta: &mut SlicerMetadata) {
    // PrusaSlicer comments look like ` key = value` (note the leading space after
    // the `;` is stripped by the parser, leaving ` key = value` or `key = value`).
    let text = text.trim_start();

    if let Some(val) = strip_prusa_key(text, PRUSA_KEY_LAYER_HEIGHT) {
        meta.layer_height = parse_f64(val);
    } else if let Some(val) = strip_prusa_key(text, PRUSA_KEY_NOZZLE_DIAMETER) {
        meta.nozzle_diameter = parse_f64(val);
    } else if let Some(val) = strip_prusa_key(text, PRUSA_KEY_FILAMENT_TYPE) {
        meta.filament_type = Some(val.trim().to_owned());
    } else if let Some(val) = strip_prusa_key(text, PRUSA_KEY_FIRST_LAYER_TEMP) {
        meta.print_temperature = parse_f64(val);
    } else if let Some(val) = strip_prusa_key(text, PRUSA_KEY_BED_TEMP) {
        meta.bed_temperature = parse_f64(val);
    } else if let Some(val) = strip_prusa_key(text, PRUSA_KEY_ESTIMATED_TIME) {
        // PrusaSlicer encodes time as "Nh Nm Ns" — convert to seconds.
        meta.estimated_time_seconds = parse_prusa_time(val);
    }
}

/// Strip a PrusaSlicer-style ` key = ` prefix and return the value, or `None`.
fn strip_prusa_key<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let rest = text.strip_prefix(key)?;
    // Allow optional whitespace around the `=` separator.
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=')?;
    Some(rest.trim_start())
}

/// Parse a PrusaSlicer time string like `"1h 23m 45s"` or `"23m 45s"` into
/// total seconds as `f64`.
///
/// Returns `None` if the string cannot be interpreted as a duration.
fn parse_prusa_time(s: &str) -> Option<f64> {
    let s = s.trim();
    // Try to parse as plain seconds first (Cura-style fallback).
    if let Ok(secs) = s.parse::<f64>() {
        return Some(secs);
    }

    let mut total: f64 = 0.0;
    let mut found = false;

    // Tokenise on whitespace; each token is either `<N>h`, `<N>m`, or `<N>s`.
    for token in s.split_whitespace() {
        if let Some(h) = token.strip_suffix('h') {
            if let Ok(n) = h.parse::<f64>() {
                total += n * 3600.0;
                found = true;
            }
        } else if let Some(m) = token.strip_suffix('m') {
            if let Ok(n) = m.parse::<f64>() {
                total += n * 60.0;
                found = true;
            }
        } else if let Some(sec) = token.strip_suffix('s') {
            if let Ok(n) = sec.parse::<f64>() {
                total += n;
                found = true;
            }
        }
    }

    if found {
        Some(total)
    } else {
        None
    }
}

/// Parse a string slice as `f64`, returning `None` on failure.
fn parse_f64(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok()
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;
    use crate::models::{GCodeCommand, Spanned};

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn comment(text: &str) -> Spanned<GCodeCommand<'_>> {
        Spanned {
            inner: GCodeCommand::Comment {
                text: Cow::Owned(text.to_owned()),
            },
            line: 1,
            byte_offset: 0,
        }
    }

    fn unknown(raw: &str) -> Spanned<GCodeCommand<'_>> {
        Spanned {
            inner: GCodeCommand::Unknown {
                raw: Cow::Owned(raw.to_owned()),
            },
            line: 1,
            byte_offset: 0,
        }
    }

    fn linear_move() -> Spanned<GCodeCommand<'static>> {
        Spanned {
            inner: GCodeCommand::LinearMove {
                x: Some(10.0),
                y: Some(10.0),
                z: None,
                e: None,
                f: None,
            },
            line: 2,
            byte_offset: 20,
        }
    }

    // ── detect_dialect ────────────────────────────────────────────────────────

    #[test]
    fn test_detect_dialect_orcaslicer_marker_returns_orca() {
        let cmds = vec![comment("Generated by OrcaSlicer 2.1.0")];
        assert_eq!(detect_dialect(&cmds), Some(Dialect::OrcaSlicer));
    }

    #[test]
    fn test_detect_dialect_prusaslicer_marker_returns_prusa() {
        let cmds = vec![comment("generated by PrusaSlicer 2.7.4")];
        assert_eq!(detect_dialect(&cmds), Some(Dialect::PrusaSlicer));
    }

    #[test]
    fn test_detect_dialect_superslicer_marker_returns_super() {
        let cmds = vec![comment("SuperSlicer-2.5.59 on 2024-01-01")];
        assert_eq!(detect_dialect(&cmds), Some(Dialect::SuperSlicer));
    }

    #[test]
    fn test_detect_dialect_cura_flavor_marker_returns_cura() {
        let cmds = vec![comment("FLAVOR:Marlin")];
        assert_eq!(detect_dialect(&cmds), Some(Dialect::Cura));
    }

    #[test]
    fn test_detect_dialect_simplify3d_marker_returns_simplify3d() {
        let cmds = vec![comment("Simplify3D Version 5.0")];
        assert_eq!(detect_dialect(&cmds), Some(Dialect::Simplify3D));
    }

    #[test]
    fn test_detect_dialect_ideamaker_marker_returns_ideamaker() {
        let cmds = vec![comment(";ideaMaker 4.3.2")];
        assert_eq!(detect_dialect(&cmds), Some(Dialect::IdeaMaker));
    }

    #[test]
    fn test_detect_dialect_no_marker_returns_none() {
        let cmds = vec![comment("some random comment"), linear_move()];
        assert_eq!(detect_dialect(&cmds), None);
    }

    #[test]
    fn test_detect_dialect_empty_input_returns_none() {
        let cmds: Vec<Spanned<GCodeCommand<'_>>> = vec![];
        assert_eq!(detect_dialect(&cmds), None);
    }

    #[test]
    fn test_detect_dialect_marker_beyond_scan_limit_returns_none() {
        // Build SCAN_LIMIT + 1 plain-move commands, then add the marker.
        let mut cmds: Vec<Spanned<GCodeCommand<'_>>> =
            (0..=SCAN_LIMIT).map(|_| linear_move()).collect();
        cmds.push(comment("OrcaSlicer marker after limit"));
        assert_eq!(detect_dialect(&cmds), None);
    }

    #[test]
    fn test_detect_dialect_marker_at_scan_limit_boundary_returns_dialect() {
        // SCAN_LIMIT - 1 plain moves, then the marker at index SCAN_LIMIT - 1 (0-based).
        let mut cmds: Vec<Spanned<GCodeCommand<'_>>> =
            (0..SCAN_LIMIT - 1).map(|_| linear_move()).collect();
        cmds.push(comment("OrcaSlicer boundary test"));
        assert_eq!(detect_dialect(&cmds), Some(Dialect::OrcaSlicer));
    }

    #[test]
    fn test_detect_dialect_unknown_raw_line_with_marker_returns_dialect() {
        let cmds = vec![unknown("; FLAVOR:Marlin")];
        assert_eq!(detect_dialect(&cmds), Some(Dialect::Cura));
    }

    #[test]
    fn test_detect_dialect_superslicer_before_prusaslicer_wins() {
        // If a comment contains "SuperSlicer" it should NOT accidentally match
        // "PrusaSlicer" — the SuperSlicer branch must fire first.
        let cmds = vec![comment("SuperSlicer (fork of PrusaSlicer)")];
        assert_eq!(detect_dialect(&cmds), Some(Dialect::SuperSlicer));
    }

    // ── extract_metadata — PrusaSlicer format ─────────────────────────────────

    #[test]
    fn test_extract_metadata_prusa_layer_height_parsed() {
        let cmds = vec![comment("layer_height = 0.2")];
        let meta = extract_metadata(&cmds);
        assert_eq!(meta.layer_height, Some(0.2));
    }

    #[test]
    fn test_extract_metadata_prusa_nozzle_diameter_parsed() {
        let cmds = vec![comment("nozzle_diameter = 0.4")];
        let meta = extract_metadata(&cmds);
        assert_eq!(meta.nozzle_diameter, Some(0.4));
    }

    #[test]
    fn test_extract_metadata_prusa_filament_type_parsed() {
        let cmds = vec![comment("filament_type = PLA")];
        let meta = extract_metadata(&cmds);
        assert_eq!(meta.filament_type.as_deref(), Some("PLA"));
    }

    #[test]
    fn test_extract_metadata_prusa_first_layer_temperature_parsed() {
        let cmds = vec![comment("first_layer_temperature = 215")];
        let meta = extract_metadata(&cmds);
        assert_eq!(meta.print_temperature, Some(215.0));
    }

    #[test]
    fn test_extract_metadata_prusa_bed_temperature_parsed() {
        let cmds = vec![comment("first_layer_bed_temperature = 60")];
        let meta = extract_metadata(&cmds);
        assert_eq!(meta.bed_temperature, Some(60.0));
    }

    #[test]
    fn test_extract_metadata_prusa_estimated_time_hms_parsed() {
        // "1h 23m 45s" → 3600 + 23*60 + 45 = 5025 s
        let cmds = vec![comment(
            "estimated printing time (normal mode) = 1h 23m 45s",
        )];
        let meta = extract_metadata(&cmds);
        assert_eq!(meta.estimated_time_seconds, Some(5025.0));
    }

    #[test]
    fn test_extract_metadata_prusa_estimated_time_minutes_only_parsed() {
        // "2m 30s" → 150 s
        let cmds = vec![comment("estimated printing time (normal mode) = 2m 30s")];
        let meta = extract_metadata(&cmds);
        assert_eq!(meta.estimated_time_seconds, Some(150.0));
    }

    // ── extract_metadata — Cura format ───────────────────────────────────────

    #[test]
    fn test_extract_metadata_cura_layer_height_parsed() {
        let cmds = vec![comment("Layer height: 0.15")];
        let meta = extract_metadata(&cmds);
        assert_eq!(meta.layer_height, Some(0.15));
    }

    #[test]
    fn test_extract_metadata_cura_nozzle_diameter_parsed() {
        let cmds = vec![comment("Nozzle diameter: 0.6")];
        let meta = extract_metadata(&cmds);
        assert_eq!(meta.nozzle_diameter, Some(0.6));
    }

    #[test]
    fn test_extract_metadata_cura_material_parsed() {
        let cmds = vec![comment("Material: PETG")];
        let meta = extract_metadata(&cmds);
        assert_eq!(meta.filament_type.as_deref(), Some("PETG"));
    }

    #[test]
    fn test_extract_metadata_cura_print_temperature_parsed() {
        let cmds = vec![comment("Extruder 1 start temperature: 240")];
        let meta = extract_metadata(&cmds);
        assert_eq!(meta.print_temperature, Some(240.0));
    }

    #[test]
    fn test_extract_metadata_cura_bed_temperature_parsed() {
        let cmds = vec![comment("Bed temperature: 80")];
        let meta = extract_metadata(&cmds);
        assert_eq!(meta.bed_temperature, Some(80.0));
    }

    #[test]
    fn test_extract_metadata_cura_print_time_seconds_parsed() {
        let cmds = vec![comment("Print time (s): 3600")];
        let meta = extract_metadata(&cmds);
        assert_eq!(meta.estimated_time_seconds, Some(3600.0));
    }

    // ── extract_metadata — edge cases ─────────────────────────────────────────

    #[test]
    fn test_extract_metadata_no_metadata_comments_returns_empty() {
        let cmds = vec![comment("G1 X10 Y10"), linear_move()];
        let meta = extract_metadata(&cmds);
        assert!(meta.is_empty());
    }

    #[test]
    fn test_extract_metadata_empty_input_returns_empty() {
        let cmds: Vec<Spanned<GCodeCommand<'_>>> = vec![];
        let meta = extract_metadata(&cmds);
        assert!(meta.is_empty());
    }

    #[test]
    fn test_extract_metadata_non_comment_commands_ignored() {
        let cmds = vec![linear_move()];
        let meta = extract_metadata(&cmds);
        assert!(meta.is_empty());
    }

    #[test]
    fn test_extract_metadata_later_value_wins_for_same_field() {
        // If the same key appears twice, the second value should overwrite the first.
        let cmds = vec![comment("layer_height = 0.2"), comment("layer_height = 0.3")];
        let meta = extract_metadata(&cmds);
        assert_eq!(meta.layer_height, Some(0.3));
    }

    // ── is_cura_layer_change ──────────────────────────────────────────────────

    #[test]
    fn test_is_cura_layer_change_layer_zero_returns_true() {
        assert!(is_cura_layer_change("LAYER:0"));
    }

    #[test]
    fn test_is_cura_layer_change_layer_n_returns_true() {
        assert!(is_cura_layer_change("LAYER:42"));
    }

    #[test]
    fn test_is_cura_layer_change_orca_marker_returns_false() {
        assert!(!is_cura_layer_change("LAYER_CHANGE"));
    }

    #[test]
    fn test_is_cura_layer_change_lowercase_returns_false() {
        assert!(!is_cura_layer_change("layer:0"));
    }

    #[test]
    fn test_is_cura_layer_change_empty_returns_false() {
        assert!(!is_cura_layer_change(""));
    }

    // ── Serialization ─────────────────────────────────────────────────────────

    #[test]
    fn test_dialect_serializes_to_snake_case_string() {
        let json = serde_json::to_string(&Dialect::OrcaSlicer).unwrap();
        assert_eq!(json, r#""orca_slicer""#);
        let json = serde_json::to_string(&Dialect::PrusaSlicer).unwrap();
        assert_eq!(json, r#""prusa_slicer""#);
        let json = serde_json::to_string(&Dialect::Cura).unwrap();
        assert_eq!(json, r#""cura""#);
    }

    #[test]
    fn test_slicer_metadata_skips_none_fields_in_json() {
        let meta = SlicerMetadata {
            layer_height: Some(0.2),
            ..Default::default()
        };
        let json = serde_json::to_string(&meta).unwrap();
        assert!(json.contains("layer_height"));
        assert!(!json.contains("nozzle_diameter"));
        assert!(!json.contains("filament_type"));
    }

    #[test]
    fn test_prusa_time_plain_seconds_parsed() {
        // Plain numeric string (Cura-compatible fallback).
        assert_eq!(parse_prusa_time("3600"), Some(3600.0));
    }

    #[test]
    fn test_prusa_time_invalid_string_returns_none() {
        assert_eq!(parse_prusa_time("not a time"), None);
    }
}
