//! Slicer dialect detection and metadata extraction.
//!
//! Analyses a parsed G-Code command stream to identify which slicer produced it
//! and extract print-relevant metadata (nozzle diameter, layer height, filament
//! type, temperatures, estimated print time).

use crate::diagnostics::{Diagnostic, Severity};
use crate::models::{GCodeCommand, Spanned};

// ──────────────────────────────────────────────────────────────────────────────
// Confidence
// ──────────────────────────────────────────────────────────────────────────────

/// How confident the detector is in its dialect identification.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub enum Confidence {
    /// No dialect could be identified.
    #[default]
    None,
    /// A single weak heuristic matched.
    Low,
    /// Multiple heuristics matched, but no definitive signature.
    Medium,
    /// A definitive comment signature was found.
    High,
}

// ──────────────────────────────────────────────────────────────────────────────
// SlicerDialect
// ──────────────────────────────────────────────────────────────────────────────

/// Known slicer dialects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum SlicerDialect {
    /// Bambu Lab `OrcaSlicer`.
    OrcaSlicer,
    /// Prusa Research `PrusaSlicer`.
    PrusaSlicer,
    /// Ultimaker Cura.
    Cura,
    /// Dialect could not be determined.
    Unknown,
}

impl SlicerDialect {
    /// Returns the metadata field names expected for this dialect.
    #[must_use]
    pub fn expected_fields(self) -> &'static [&'static str] {
        match self {
            Self::OrcaSlicer | Self::PrusaSlicer => &[
                "nozzle_diameter_mm",
                "layer_height_mm",
                "filament_type",
                "first_layer_bed_temperature",
                "hotend_temperature",
                "estimated_time_seconds",
            ],
            Self::Cura => &[
                "nozzle_diameter_mm",
                "layer_height_mm",
                "filament_type",
                "bed_temperature",
                "hotend_temperature",
                "print_time",
            ],
            Self::Unknown => &[],
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// SlicerMetadata
// ──────────────────────────────────────────────────────────────────────────────

/// Extracted slicer metadata from G-Code comments.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SlicerMetadata {
    /// Detected slicer dialect.
    pub dialect: SlicerDialect,
    /// Detection confidence level.
    pub confidence: Confidence,
    /// Slicer version string, if found.
    pub slicer_version: Option<String>,
    /// Nozzle diameter in mm.
    pub nozzle_diameter_mm: Option<f64>,
    /// Layer height in mm.
    pub layer_height_mm: Option<f64>,
    /// Filament type (e.g. `"PLA"`, `"PETG"`).
    pub filament_type: Option<String>,
    /// Bed temperature for the first layer in degrees Celsius.
    pub bed_temperature: Option<f64>,
    /// Hotend temperature in degrees Celsius.
    pub hotend_temperature: Option<f64>,
    /// Estimated print time in seconds.
    pub estimated_time_seconds: Option<f64>,
}

impl Default for SlicerMetadata {
    fn default() -> Self {
        Self {
            dialect: SlicerDialect::Unknown,
            confidence: Confidence::default(),
            slicer_version: None,
            nozzle_diameter_mm: None,
            layer_height_mm: None,
            filament_type: None,
            bed_temperature: None,
            hotend_temperature: None,
            estimated_time_seconds: None,
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// DialectResult
// ──────────────────────────────────────────────────────────────────────────────

/// Result of dialect detection: metadata plus any diagnostics generated.
#[derive(Debug, Clone)]
pub struct DialectResult {
    /// Extracted slicer metadata.
    pub metadata: SlicerMetadata,
    /// Diagnostics generated during detection.
    pub diagnostics: Vec<Diagnostic>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Detection
// ──────────────────────────────────────────────────────────────────────────────

/// Detect the slicer dialect and extract metadata from a parsed command stream.
///
/// When `dialect_override` is `Some`, it is used directly (with `High` confidence)
/// and missing-metadata warnings (W005) are suppressed.
#[must_use]
pub fn detect_dialect(
    commands: &[Spanned<GCodeCommand<'_>>],
    dialect_override: Option<SlicerDialect>,
) -> DialectResult {
    let mut metadata = SlicerMetadata::default();
    let mut diagnostics = Vec::new();

    // Phase 1: Comment signature scan (first 100 lines)
    let head_limit = commands.len().min(100);
    let mut phase1_dialect: Option<SlicerDialect> = None;
    let mut phase1_line: u32 = 0;

    if dialect_override.is_none() {
        for cmd in &commands[..head_limit] {
            if let GCodeCommand::Comment { text } = &cmd.inner {
                let lower = text.to_lowercase();
                if lower.contains("generated by orcaslicer") {
                    phase1_dialect = Some(SlicerDialect::OrcaSlicer);
                    phase1_line = cmd.line;
                    metadata.slicer_version = extract_version_after(text, "OrcaSlicer");
                    break;
                } else if lower.contains("generated by prusaslicer") {
                    phase1_dialect = Some(SlicerDialect::PrusaSlicer);
                    phase1_line = cmd.line;
                    metadata.slicer_version = extract_version_after(text, "PrusaSlicer");
                    break;
                } else if lower.contains("flavor:") || lower.contains("generated with cura") {
                    phase1_dialect = Some(SlicerDialect::Cura);
                    phase1_line = cmd.line;
                    if lower.contains("generated with cura") {
                        metadata.slicer_version = extract_version_after(text, "Cura_SteamEngine");
                    }
                    break;
                }
            }
        }
    }

    let (dialect, confidence) = if let Some(ovr) = dialect_override {
        (ovr, Confidence::High)
    } else if let Some(d) = phase1_dialect {
        (d, Confidence::High)
    } else {
        // Phase 2: M-code heuristics
        heuristic_detection(commands)
    };

    metadata.dialect = dialect;
    metadata.confidence = confidence;

    // Extract metadata based on dialect
    extract_metadata(commands, dialect, &mut metadata);

    // Emit I004 diagnostic
    let detection_line = if dialect_override.is_some() {
        0
    } else if phase1_dialect.is_some() {
        phase1_line
    } else {
        0
    };

    if dialect != SlicerDialect::Unknown {
        diagnostics.push(Diagnostic {
            severity: Severity::Info,
            line: detection_line,
            code: "I004",
            message: format!("slicer dialect detected: {dialect:?} (confidence: {confidence:?})",),
        });
    }

    // W005: expected metadata missing (suppressed when dialect_override is Some)
    if dialect_override.is_none() && dialect != SlicerDialect::Unknown {
        let expected = dialect.expected_fields();
        let missing = collect_missing_fields(&metadata, expected);
        for field in missing {
            diagnostics.push(Diagnostic {
                severity: Severity::Warning,
                line: 0,
                code: "W005",
                message: format!("expected metadata field missing: {field}"),
            });
        }
    }

    DialectResult {
        metadata,
        diagnostics,
    }
}

/// Run Phase 2 heuristic detection using M-code patterns.
fn heuristic_detection(commands: &[Spanned<GCodeCommand<'_>>]) -> (SlicerDialect, Confidence) {
    let mut orca_prusa_signals: u32 = 0;
    let mut cura_signals: u32 = 0;
    let mut prusa_signals: u32 = 0;

    for cmd in commands {
        match &cmd.inner {
            GCodeCommand::MetaCommand { code: 73, params } => {
                if params.contains('R') {
                    // M73 with P+R → orca/prusa
                    orca_prusa_signals += 1;
                } else if params.contains('P') {
                    // M73 with P only → cura
                    cura_signals += 1;
                }
            }
            GCodeCommand::MetaCommand { code: 900, .. } => {
                // M900 (linear advance) → prusa signal
                prusa_signals += 1;
            }
            GCodeCommand::Comment { text } if text.starts_with("TYPE:") => {
                // ;TYPE: comments → orca/prusa
                orca_prusa_signals += 1;
            }
            _ => {}
        }
    }

    // Determine the best-matching dialect
    let orca_prusa_total = orca_prusa_signals + prusa_signals;
    let total_signals = orca_prusa_total + cura_signals;

    if total_signals == 0 {
        return (SlicerDialect::Unknown, Confidence::None);
    }

    // Pick the dialect with the most signals
    if cura_signals > orca_prusa_total {
        let conf = if cura_signals >= 2 {
            Confidence::Medium
        } else {
            Confidence::Low
        };
        (SlicerDialect::Cura, conf)
    } else if prusa_signals > 0 {
        // Prusa-specific M900 tips the balance
        let conf = if orca_prusa_total >= 2 {
            Confidence::Medium
        } else {
            Confidence::Low
        };
        (SlicerDialect::PrusaSlicer, conf)
    } else {
        // orca/prusa signals without prusa-specific → OrcaSlicer
        let conf = if orca_prusa_signals >= 2 {
            Confidence::Medium
        } else {
            Confidence::Low
        };
        (SlicerDialect::OrcaSlicer, conf)
    }
}

/// Extract metadata fields from the command stream based on dialect.
fn extract_metadata(
    commands: &[Spanned<GCodeCommand<'_>>],
    dialect: SlicerDialect,
    metadata: &mut SlicerMetadata,
) {
    match dialect {
        SlicerDialect::OrcaSlicer | SlicerDialect::PrusaSlicer => {
            // Find CONFIG_BLOCK_START marker scanning backwards; start a
            // few lines earlier to catch metadata just before the block
            // (e.g. `estimated printing time` which precedes CONFIG_BLOCK).
            // Fall back to last 1000 commands for files without the marker.
            let config_start = match commands.iter().rposition(|c| {
                matches!(
                    &c.inner,
                    GCodeCommand::Comment { text } if text.contains("CONFIG_BLOCK_START")
                )
            }) {
                Some(i) => i.saturating_sub(10),
                None => commands.len().saturating_sub(1000),
            };
            for cmd in &commands[config_start..] {
                if let GCodeCommand::Comment { text } = &cmd.inner {
                    extract_orca_prusa_field(text, metadata);
                }
            }
        }
        SlicerDialect::Cura => {
            // Header: first 100 lines
            let limit = commands.len().min(100);
            for cmd in &commands[..limit] {
                if let GCodeCommand::Comment { text } = &cmd.inner {
                    extract_cura_field(text, metadata);
                }
            }
        }
        SlicerDialect::Unknown => {}
    }
}

/// Extract a single OrcaSlicer/PrusaSlicer metadata field from a comment line.
///
/// Expected format: ` key = value` (with leading space after semicolon stripped
/// by the parser). Uses exact key matching to avoid sub-key collisions.
fn extract_orca_prusa_field(text: &str, metadata: &mut SlicerMetadata) {
    let trimmed = text.trim();

    // Split on first '=' only
    if let Some((key, value)) = trimmed.split_once('=') {
        let key = key.trim();
        let value = value.trim();

        match key {
            "nozzle_diameter" => {
                metadata.nozzle_diameter_mm = value.parse::<f64>().ok();
            }
            "layer_height" => {
                metadata.layer_height_mm = value.parse::<f64>().ok();
            }
            "filament_type" => {
                metadata.filament_type = Some(value.to_string());
            }
            "first_layer_bed_temperature" => {
                metadata.bed_temperature = value.parse::<f64>().ok();
            }
            "nozzle_temperature" => {
                metadata.hotend_temperature = value.parse::<f64>().ok();
            }
            _ => {}
        }
    }

    // Estimated printing time has a different format:
    // "; estimated printing time (normal mode) = 2h 26m 25s"
    if trimmed.starts_with("estimated printing time") {
        if let Some((_, value)) = trimmed.split_once('=') {
            metadata.estimated_time_seconds = parse_time_estimate(value.trim());
        }
    }
}

/// Extract a single Cura metadata field from a comment line.
///
/// Expected formats:
/// - `Nozzle size: 0.4`
/// - `Layer height: 0.2`
/// - `PRINT.TIME:3600`
/// - `Filament type: PLA`
/// - `BUILD_PLATE.INITIAL_TEMPERATURE:60`
/// - `EXTRUDER.INITIAL_TEMPERATURE:210`
fn extract_cura_field(text: &str, metadata: &mut SlicerMetadata) {
    let trimmed = text.trim();

    if let Some(val) = trimmed.strip_prefix("Nozzle size:") {
        metadata.nozzle_diameter_mm = val.trim().parse::<f64>().ok();
    } else if let Some(val) = trimmed.strip_prefix("Layer height:") {
        metadata.layer_height_mm = val.trim().parse::<f64>().ok();
    } else if let Some(val) = trimmed.strip_prefix("PRINT.TIME:") {
        metadata.estimated_time_seconds = val.trim().parse::<f64>().ok();
    } else if let Some(val) = trimmed.strip_prefix("Filament type:") {
        metadata.filament_type = Some(val.trim().to_string());
    } else if let Some(val) = trimmed.strip_prefix("BUILD_PLATE.INITIAL_TEMPERATURE:") {
        metadata.bed_temperature = val.trim().parse::<f64>().ok();
    } else if let Some(val) = trimmed.strip_prefix("EXTRUDER.INITIAL_TEMPERATURE:") {
        metadata.hotend_temperature = val.trim().parse::<f64>().ok();
    }
}

/// Parse a time estimate string like "2h 26m 25s" into total seconds.
fn parse_time_estimate(text: &str) -> Option<f64> {
    let mut total = 0.0_f64;
    let mut current_num = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() {
            current_num.push(c);
        } else if !current_num.is_empty() {
            let n: f64 = current_num.parse().ok()?;
            match c {
                'h' => total += n * 3600.0,
                'm' => total += n * 60.0,
                's' => total += n,
                _ => {}
            }
            current_num.clear();
        }
    }
    if total > 0.0 {
        Some(total)
    } else {
        None
    }
}

/// Extract a version number that follows a keyword in a comment.
fn extract_version_after(text: &str, keyword: &str) -> Option<String> {
    let idx = text.find(keyword)?;
    let after = &text[idx + keyword.len()..];
    let trimmed = after.trim();
    // Take the first whitespace-delimited token as the version
    let version = trimmed.split_whitespace().next()?;
    if version.is_empty() {
        None
    } else {
        Some(version.to_string())
    }
}

/// Collect names of expected metadata fields that are missing.
fn collect_missing_fields(
    metadata: &SlicerMetadata,
    expected: &[&'static str],
) -> Vec<&'static str> {
    let mut missing = Vec::new();
    for &field in expected {
        let present = match field {
            "nozzle_diameter_mm" => metadata.nozzle_diameter_mm.is_some(),
            "layer_height_mm" => metadata.layer_height_mm.is_some(),
            "filament_type" => metadata.filament_type.is_some(),
            "first_layer_bed_temperature" | "bed_temperature" => metadata.bed_temperature.is_some(),
            "hotend_temperature" => metadata.hotend_temperature.is_some(),
            "estimated_time_seconds" | "print_time" => metadata.estimated_time_seconds.is_some(),
            _ => true,
        };
        if !present {
            missing.push(field);
        }
    }
    missing
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    /// Helper to build a `Spanned<GCodeCommand>` comment at a given line.
    fn comment(line: u32, text: &str) -> Spanned<GCodeCommand<'_>> {
        Spanned {
            inner: GCodeCommand::Comment {
                text: Cow::Borrowed(text),
            },
            line,
            byte_offset: 0,
        }
    }

    /// Helper to build a `Spanned<GCodeCommand>` meta command.
    fn meta(line: u32, code: u16, params: &str) -> Spanned<GCodeCommand<'_>> {
        Spanned {
            inner: GCodeCommand::MetaCommand {
                code,
                params: Cow::Borrowed(params),
            },
            line,
            byte_offset: 0,
        }
    }

    // ── Confidence ordering ──────────────────────────────────────────────

    #[test]
    fn given_confidence_enum_when_compared_then_none_is_lowest() {
        assert!(Confidence::None < Confidence::Low);
        assert!(Confidence::Low < Confidence::Medium);
        assert!(Confidence::Medium < Confidence::High);
    }

    // ── expected_fields ──────────────────────────────────────────────────

    #[test]
    fn given_orcaslicer_when_expected_fields_then_returns_six_fields() {
        let fields = SlicerDialect::OrcaSlicer.expected_fields();
        assert_eq!(fields.len(), 6);
        assert!(fields.contains(&"nozzle_diameter_mm"));
        assert!(fields.contains(&"estimated_time_seconds"));
    }

    #[test]
    fn given_cura_when_expected_fields_then_returns_six_fields() {
        let fields = SlicerDialect::Cura.expected_fields();
        assert_eq!(fields.len(), 6);
        assert!(fields.contains(&"print_time"));
    }

    #[test]
    fn given_unknown_when_expected_fields_then_returns_empty() {
        assert!(SlicerDialect::Unknown.expected_fields().is_empty());
    }

    // ── Phase 1: comment signature detection ─────────────────────────────

    #[test]
    fn given_orcaslicer_header_when_detected_then_high_confidence() {
        let commands = vec![
            comment(1, " generated by OrcaSlicer 2.1.0 on 2024-01-15"),
            comment(2, " some other comment"),
        ];
        let result = detect_dialect(&commands, None);
        assert_eq!(result.metadata.dialect, SlicerDialect::OrcaSlicer);
        assert_eq!(result.metadata.confidence, Confidence::High);
        assert_eq!(result.metadata.slicer_version.as_deref(), Some("2.1.0"));
    }

    #[test]
    fn given_prusaslicer_header_when_detected_then_high_confidence() {
        let commands = vec![comment(
            1,
            " generated by PrusaSlicer 2.7.1+linux on 2024-03-01",
        )];
        let result = detect_dialect(&commands, None);
        assert_eq!(result.metadata.dialect, SlicerDialect::PrusaSlicer);
        assert_eq!(result.metadata.confidence, Confidence::High);
        assert_eq!(
            result.metadata.slicer_version.as_deref(),
            Some("2.7.1+linux")
        );
    }

    #[test]
    fn given_cura_flavor_comment_when_detected_then_high_confidence() {
        let commands = vec![comment(1, "FLAVOR:Marlin")];
        let result = detect_dialect(&commands, None);
        assert_eq!(result.metadata.dialect, SlicerDialect::Cura);
        assert_eq!(result.metadata.confidence, Confidence::High);
    }

    #[test]
    fn given_cura_generated_with_comment_when_detected_then_extracts_version() {
        let commands = vec![comment(1, " Generated with Cura_SteamEngine 5.6.0")];
        let result = detect_dialect(&commands, None);
        assert_eq!(result.metadata.dialect, SlicerDialect::Cura);
        assert_eq!(result.metadata.slicer_version.as_deref(), Some("5.6.0"));
    }

    #[test]
    fn given_case_insensitive_header_when_detected_then_matches() {
        let commands = vec![comment(1, " GENERATED BY ORCASLICER 2.0.0")];
        let result = detect_dialect(&commands, None);
        assert_eq!(result.metadata.dialect, SlicerDialect::OrcaSlicer);
        assert_eq!(result.metadata.confidence, Confidence::High);
    }

    // ── Phase 2: heuristic detection ─────────────────────────────────────

    #[test]
    fn given_m73_with_p_and_r_when_heuristic_then_orca_signal() {
        let commands = vec![meta(1, 73, "P50 R30"), meta(2, 73, "P75 R15")];
        let result = detect_dialect(&commands, None);
        assert_eq!(result.metadata.dialect, SlicerDialect::OrcaSlicer);
        assert_eq!(result.metadata.confidence, Confidence::Medium);
    }

    #[test]
    fn given_m73_with_p_only_when_heuristic_then_cura_signal() {
        let commands = vec![meta(1, 73, "P50"), meta(2, 73, "P75")];
        let result = detect_dialect(&commands, None);
        assert_eq!(result.metadata.dialect, SlicerDialect::Cura);
        assert_eq!(result.metadata.confidence, Confidence::Medium);
    }

    #[test]
    fn given_m900_when_heuristic_then_prusaslicer_signal() {
        let commands = vec![meta(1, 900, "K0.04"), comment(2, "TYPE:External perimeter")];
        let result = detect_dialect(&commands, None);
        assert_eq!(result.metadata.dialect, SlicerDialect::PrusaSlicer);
        assert_eq!(result.metadata.confidence, Confidence::Medium);
    }

    #[test]
    fn given_single_type_comment_when_heuristic_then_low_confidence() {
        let commands = vec![comment(1, "TYPE:Infill")];
        let result = detect_dialect(&commands, None);
        assert_eq!(result.metadata.dialect, SlicerDialect::OrcaSlicer);
        assert_eq!(result.metadata.confidence, Confidence::Low);
    }

    #[test]
    fn given_no_signals_when_heuristic_then_unknown() {
        let commands = vec![meta(1, 104, "S200")];
        let result = detect_dialect(&commands, None);
        assert_eq!(result.metadata.dialect, SlicerDialect::Unknown);
        assert_eq!(result.metadata.confidence, Confidence::None);
    }

    // ── dialect_override ─────────────────────────────────────────────────

    #[test]
    fn given_dialect_override_when_detected_then_uses_override() {
        let commands = vec![comment(1, " generated by OrcaSlicer 2.1.0")];
        let result = detect_dialect(&commands, Some(SlicerDialect::PrusaSlicer));
        assert_eq!(result.metadata.dialect, SlicerDialect::PrusaSlicer);
        assert_eq!(result.metadata.confidence, Confidence::High);
    }

    #[test]
    fn given_dialect_override_when_metadata_missing_then_no_w005() {
        let commands = vec![comment(1, " just a comment")];
        let result = detect_dialect(&commands, Some(SlicerDialect::OrcaSlicer));
        let w005_count = result
            .diagnostics
            .iter()
            .filter(|d| d.code == "W005")
            .count();
        assert_eq!(w005_count, 0);
    }

    // ── Metadata extraction: OrcaSlicer/PrusaSlicer footer ───────────────

    #[test]
    fn given_orca_footer_when_extracted_then_populates_all_fields() {
        let mut commands = vec![comment(1, " generated by OrcaSlicer 2.1.0")];
        // Pad to push footer into last 100
        for i in 2..50 {
            commands.push(meta(i, 1, "X10 Y10 E0.5"));
        }
        commands.push(comment(50, " nozzle_diameter = 0.4"));
        commands.push(comment(51, " layer_height = 0.2"));
        commands.push(comment(52, " filament_type = PLA"));
        commands.push(comment(53, " first_layer_bed_temperature = 55"));
        commands.push(comment(54, " nozzle_temperature = 210"));
        commands.push(comment(
            55,
            " estimated printing time (normal mode) = 2h 26m 25s",
        ));

        let result = detect_dialect(&commands, None);
        let m = &result.metadata;
        assert_eq!(m.nozzle_diameter_mm, Some(0.4));
        assert_eq!(m.layer_height_mm, Some(0.2));
        assert_eq!(m.filament_type.as_deref(), Some("PLA"));
        assert_eq!(m.bed_temperature, Some(55.0));
        assert_eq!(m.hotend_temperature, Some(210.0));
        assert_eq!(m.estimated_time_seconds, Some(8785.0));
    }

    #[test]
    fn given_orca_footer_when_subkey_present_then_exact_match_only() {
        // "nozzle_temperature_initial_layer" should NOT match "nozzle_temperature"
        let mut commands = vec![comment(1, " generated by OrcaSlicer 2.0.0")];
        commands.push(comment(2, " nozzle_temperature_initial_layer = 215"));
        commands.push(comment(3, " nozzle_temperature = 210"));

        let result = detect_dialect(&commands, None);
        assert_eq!(result.metadata.hotend_temperature, Some(210.0));
    }

    // ── Metadata extraction: Cura header ─────────────────────────────────

    #[test]
    fn given_cura_header_when_extracted_then_populates_all_fields() {
        let commands = vec![
            comment(1, "FLAVOR:Marlin"),
            comment(2, "Nozzle size: 0.4"),
            comment(3, "Layer height: 0.2"),
            comment(4, "PRINT.TIME:3600"),
            comment(5, "Filament type: PLA"),
            comment(6, "BUILD_PLATE.INITIAL_TEMPERATURE:60"),
            comment(7, "EXTRUDER.INITIAL_TEMPERATURE:210"),
        ];

        let result = detect_dialect(&commands, None);
        let m = &result.metadata;
        assert_eq!(m.nozzle_diameter_mm, Some(0.4));
        assert_eq!(m.layer_height_mm, Some(0.2));
        assert_eq!(m.estimated_time_seconds, Some(3600.0));
        assert_eq!(m.filament_type.as_deref(), Some("PLA"));
        assert_eq!(m.bed_temperature, Some(60.0));
        assert_eq!(m.hotend_temperature, Some(210.0));
    }

    // ── Diagnostics ──────────────────────────────────────────────────────

    #[test]
    fn given_detected_dialect_when_result_then_emits_i004() {
        let commands = vec![comment(1, " generated by OrcaSlicer 2.0.0")];
        let result = detect_dialect(&commands, None);
        let i004 = result.diagnostics.iter().find(|d| d.code == "I004");
        assert!(i004.is_some());
        assert_eq!(i004.unwrap().severity, Severity::Info);
        assert_eq!(i004.unwrap().line, 1);
    }

    #[test]
    fn given_unknown_dialect_when_result_then_no_i004() {
        let commands = vec![meta(1, 104, "S200")];
        let result = detect_dialect(&commands, None);
        let i004 = result.diagnostics.iter().find(|d| d.code == "I004");
        assert!(i004.is_none());
    }

    #[test]
    fn given_missing_metadata_when_detected_then_emits_w005() {
        let commands = vec![comment(1, " generated by OrcaSlicer 2.0.0")];
        let result = detect_dialect(&commands, None);
        let w005s: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|d| d.code == "W005")
            .collect();
        // All 6 fields missing
        assert_eq!(w005s.len(), 6);
        assert!(w005s.iter().all(|d| d.severity == Severity::Warning));
    }

    #[test]
    fn given_all_metadata_present_when_detected_then_no_w005() {
        let mut commands = vec![comment(1, " generated by OrcaSlicer 2.0.0")];
        commands.push(comment(2, " nozzle_diameter = 0.4"));
        commands.push(comment(3, " layer_height = 0.2"));
        commands.push(comment(4, " filament_type = PLA"));
        commands.push(comment(5, " first_layer_bed_temperature = 55"));
        commands.push(comment(6, " nozzle_temperature = 210"));
        commands.push(comment(
            7,
            " estimated printing time (normal mode) = 2h 26m 25s",
        ));

        let result = detect_dialect(&commands, None);
        let w005s: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|d| d.code == "W005")
            .collect();
        assert_eq!(w005s.len(), 0);
    }

    // ── Serialization ────────────────────────────────────────────────────

    #[test]
    fn given_confidence_when_serialized_then_json_string() {
        let json = serde_json::to_string(&Confidence::High).unwrap();
        assert_eq!(json, r#""High""#);
    }

    #[test]
    fn given_slicer_dialect_when_serialized_then_json_string() {
        let json = serde_json::to_string(&SlicerDialect::OrcaSlicer).unwrap();
        assert_eq!(json, r#""OrcaSlicer""#);
    }

    #[test]
    fn given_metadata_when_serialized_then_valid_json() {
        let m = SlicerMetadata {
            dialect: SlicerDialect::Cura,
            confidence: Confidence::High,
            nozzle_diameter_mm: Some(0.4),
            ..SlicerMetadata::default()
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"Cura\""));
        assert!(json.contains("0.4"));
    }

    // ── Edge cases ───────────────────────────────────────────────────────

    #[test]
    fn given_empty_commands_when_detected_then_unknown() {
        let result = detect_dialect(&[], None);
        assert_eq!(result.metadata.dialect, SlicerDialect::Unknown);
        assert_eq!(result.metadata.confidence, Confidence::None);
    }

    #[test]
    fn given_signature_beyond_100_lines_when_detected_then_not_found_in_phase1() {
        let mut commands: Vec<Spanned<GCodeCommand<'_>>> = Vec::new();
        for i in 1..=100 {
            commands.push(meta(i, 1, "X10 Y10"));
        }
        commands.push(comment(101, " generated by OrcaSlicer 2.0.0"));
        let result = detect_dialect(&commands, None);
        // Phase 1 won't find it; heuristics won't match either
        assert_eq!(result.metadata.dialect, SlicerDialect::Unknown);
    }
}
