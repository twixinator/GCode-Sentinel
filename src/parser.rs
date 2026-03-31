//! Zero-copy, line-oriented G-Code parser.
//!
//! # Design
//!
//! G-Code is strictly line-oriented with no recursion, so a hand-written
//! scanner is faster and simpler than any combinator library.  Every string
//! field in the returned AST is a [`Cow::Borrowed`] slice directly into the
//! caller's input buffer — no heap allocation occurs unless a number parse
//! fails and we need to own the error value.
//!
//! # Byte-offset tracking
//!
//! [`str::lines`] silently strips `\r\n` endings and does not expose the
//! original line length, making it impossible to track byte offsets accurately.
//! This module instead splits on `\n` manually and strips a trailing `\r` from
//! each line before parsing, so every line's byte offset is exact.

#![warn(clippy::pedantic)]

use std::borrow::Cow;

use crate::models::{GCodeCommand, ParseError, Spanned};

// ─────────────────────────────────────────────────────────────────────────────
// Public API
// ─────────────────────────────────────────────────────────────────────────────

/// Parse a single line of G-Code and return a [`Spanned`] command.
///
/// The `line_number` and `byte_offset` parameters are injected by the caller
/// (usually [`parse_streaming`]) so that this function can be used both as part
/// of a streaming parse and as a one-shot helper in unit tests.
///
/// # Errors
///
/// Returns [`ParseError::InvalidNumber`] when a parameter that must be a
/// floating-point number cannot be parsed.  Completely unrecognised lines are
/// **not** errors — they are returned as [`GCodeCommand::Unknown`].
pub fn parse_line(
    input: &str,
    line_number: u32,
    byte_offset: u64,
) -> Result<Spanned<GCodeCommand<'_>>, ParseError> {
    let cmd = parse_line_inner(input, line_number)?;
    Ok(Spanned {
        inner: cmd,
        line: line_number,
        byte_offset,
    })
}

/// Returns a lazy iterator that yields one [`Result`] per line.
///
/// Parsing continues even after an error line — callers that need all
/// diagnostics in a single pass should collect the iterator and inspect every
/// item.
///
/// # Examples
///
/// ```
/// use gcode_sentinel::parser::parse_streaming;
///
/// let src = "G1 X10 Y20\n;comment\n";
/// let results: Vec<_> = parse_streaming(src).collect();
/// assert_eq!(results.len(), 2);
/// ```
pub fn parse_streaming(
    input: &'_ str,
) -> impl Iterator<Item = Result<Spanned<GCodeCommand<'_>>, ParseError>> + '_ {
    StreamingParser::new(input)
}

/// Parse the entire input and collect results into a [`Vec`].
///
/// Stops at the first [`ParseError`] and returns it.  Use [`parse_streaming`]
/// if you need recoverable (per-line) error handling.
///
/// # Errors
///
/// Returns the first [`ParseError`] encountered while parsing any line.
pub fn parse_all(input: &'_ str) -> Result<Vec<Spanned<GCodeCommand<'_>>>, ParseError> {
    parse_streaming(input).collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Streaming iterator
// ─────────────────────────────────────────────────────────────────────────────

struct StreamingParser<'a> {
    /// The full source buffer.
    src: &'a str,
    /// Byte position of the start of the next line to yield.
    pos: usize,
    /// 1-based line counter.
    line: u32,
}

impl<'a> StreamingParser<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src,
            pos: 0,
            line: 1,
        }
    }
}

impl<'a> Iterator for StreamingParser<'a> {
    type Item = Result<Spanned<GCodeCommand<'a>>, ParseError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos > self.src.len() {
            return None;
        }
        // Find the end of the current line.
        let rest = &self.src[self.pos..];
        if rest.is_empty() {
            // Consumed the last byte on the previous iteration.
            return None;
        }

        let (raw_line, advance) = match rest.find('\n') {
            Some(nl) => (&rest[..nl], nl + 1),
            // No newline: last line of the file, possibly without a trailing newline.
            None => (rest, rest.len()),
        };

        let byte_offset = self.pos as u64;
        let line_number = self.line;

        self.pos += advance;
        self.line += 1;

        // Strip optional \r so \r\n files work correctly.
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);

        Some(parse_line(line, line_number, byte_offset))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Core line parser
// ─────────────────────────────────────────────────────────────────────────────

/// Parse the content of a single (already-split, `\r`-stripped) line.
// G-code dispatch is an exhaustive match table; adding G2/G3 arms pushed it
// just over the 100-line limit.  Extracting sub-functions would scatter the
// one-to-one letter→variant mapping across the file without clarity gain.
#[allow(clippy::too_many_lines)]
fn parse_line_inner(line: &str, line_number: u32) -> Result<GCodeCommand<'_>, ParseError> {
    let trimmed = line.trim_start();

    // ── Empty / whitespace-only ────────────────────────────────────────────
    if trimmed.is_empty() {
        return Ok(GCodeCommand::Unknown {
            raw: Cow::Borrowed(""),
        });
    }

    // ── Parenthesised comment  ( text ) ───────────────────────────────────
    if trimmed.starts_with('(') {
        // Grab everything inside the parens; if there is no closing paren,
        // treat the whole trimmed string as the comment text.
        let text = trimmed
            .strip_prefix('(')
            .and_then(|s| s.strip_suffix(')'))
            .map_or(trimmed, |inner| inner.trim());
        // text is a sub-slice of `line`, so Borrowed is safe.
        return Ok(GCodeCommand::Comment {
            text: Cow::Borrowed(text),
        });
    }

    // ── Semicolon comment ─────────────────────────────────────────────────
    if let Some(text) = trimmed.strip_prefix(';') {
        // Everything after the leading ';' is the comment text.
        return Ok(GCodeCommand::Comment {
            text: Cow::Borrowed(text),
        });
    }

    // ── Command line ──────────────────────────────────────────────────────
    // Split at the first ';' to strip any inline comment before tokenising.
    let (cmd_part, _comment) = split_inline_comment(trimmed);
    let cmd_part = cmd_part.trim_end();

    // Try to parse the leading token as G<n> or M<n>.
    let Some((letter, code, after_code)) = parse_command_token(cmd_part) else {
        // Not a G/M code → Unknown (Klipper macros, OrcaSlicer thumbnail data, …)
        return Ok(GCodeCommand::Unknown {
            raw: Cow::Borrowed(trimmed),
        });
    };

    let params_raw = after_code.trim_start();

    match (letter, code) {
        // ── G0 – Rapid move ───────────────────────────────────────────────
        (b'G', 0) => {
            let p = parse_xyzef(params_raw, line_number)?;
            Ok(GCodeCommand::RapidMove {
                x: p.x_pos,
                y: p.y_pos,
                z: p.z_pos,
                f: p.feed,
            })
        }
        // ── G1 – Linear move ──────────────────────────────────────────────
        (b'G', 1) => {
            let p = parse_xyzef(params_raw, line_number)?;
            Ok(GCodeCommand::LinearMove {
                x: p.x_pos,
                y: p.y_pos,
                z: p.z_pos,
                e: p.extrude,
                f: p.feed,
            })
        }
        // ── G2 – Clockwise arc move ───────────────────────────────────────
        (b'G', 2) => {
            let p = parse_xyzef_ij(params_raw, line_number)?;
            Ok(GCodeCommand::ArcMoveCW {
                x: p.x_pos,
                y: p.y_pos,
                z: p.z_pos,
                e: p.extrude,
                f: p.feed,
                i: p.i_offset,
                j: p.j_offset,
            })
        }
        // ── G3 – Counter-clockwise arc move ───────────────────────────────
        (b'G', 3) => {
            let p = parse_xyzef_ij(params_raw, line_number)?;
            Ok(GCodeCommand::ArcMoveCCW {
                x: p.x_pos,
                y: p.y_pos,
                z: p.z_pos,
                e: p.extrude,
                f: p.feed,
                i: p.i_offset,
                j: p.j_offset,
            })
        }
        // ── G90 – Set absolute ────────────────────────────────────────────
        (b'G', 90) => Ok(GCodeCommand::SetAbsolute),
        // ── G91 – Set relative ────────────────────────────────────────────
        (b'G', 91) => Ok(GCodeCommand::SetRelative),
        // ── G92 – Set position ────────────────────────────────────────────
        (b'G', 92) => {
            let p = parse_xyzef(params_raw, line_number)?;
            Ok(GCodeCommand::SetPosition {
                x: p.x_pos,
                y: p.y_pos,
                z: p.z_pos,
                e: p.extrude,
            })
        }
        // ── All other G-codes ─────────────────────────────────────────────
        (b'G', n) => {
            let code = u16::try_from(n).unwrap_or(u16::MAX);
            Ok(GCodeCommand::GCommand {
                code,
                params: Cow::Borrowed(params_raw),
            })
        }
        // ── M-codes ───────────────────────────────────────────────────────
        (b'M', n) => {
            let code = u16::try_from(n).unwrap_or(u16::MAX);
            Ok(GCodeCommand::MetaCommand {
                code,
                params: Cow::Borrowed(params_raw),
            })
        }
        // Unreachable: parse_command_token only returns b'G' or b'M'.
        _ => Ok(GCodeCommand::Unknown {
            raw: Cow::Borrowed(trimmed),
        }),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Token helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Split a line at the first unquoted `;`, returning `(before, after_semicolon)`.
///
/// The semicolon itself is not included in either slice.
fn split_inline_comment(s: &str) -> (&str, &str) {
    match s.find(';') {
        Some(pos) => (&s[..pos], &s[pos + 1..]),
        None => (s, ""),
    }
}

/// Attempt to parse the leading command token of a line.
///
/// Returns `Some((letter, code, rest))` where:
/// - `letter` is `b'G'` or `b'M'` (always upper-case)
/// - `code` is the parsed integer
/// - `rest` is the remaining line content after the token
///
/// Returns `None` if the line does not start with a G/M followed by digits.
fn parse_command_token(s: &str) -> Option<(u8, u32, &str)> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let letter = match bytes[0] {
        b'G' | b'g' => b'G',
        b'M' | b'm' => b'M',
        _ => return None,
    };

    // Consume digits immediately following the letter.
    let digit_start = 1;
    let digit_end = bytes[digit_start..]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count()
        + digit_start;

    if digit_end == digit_start {
        // No digits — not a command token (e.g. a bare 'G' is nonsense).
        return None;
    }

    // Require that the character immediately after the digits is either a
    // space, end-of-string, or a parameter letter.  This prevents matching
    // something like `GRID_LEVELING` as `G` + `RID_LEVELING`.
    if let Some(&next_byte) = bytes.get(digit_end) {
        if next_byte.is_ascii_alphanumeric() && !next_byte.is_ascii_digit() {
            // The token runs on into a longer identifier — not a G/M code.
            return None;
        }
    }

    let code_str = &s[digit_start..digit_end];
    let code: u32 = code_str.parse().ok()?;
    let rest = &s[digit_end..];
    Some((letter, code, rest))
}

// ─────────────────────────────────────────────────────────────────────────────
// Parameter parser
// ─────────────────────────────────────────────────────────────────────────────

/// Parsed axis and feed-rate values from a G0 / G1 / G92 parameter string.
///
/// Using a named struct instead of a bare tuple avoids the
/// `clippy::many_single_char_names` warning that fires when five single-letter
/// bindings are in scope at once.
struct MoveParams {
    x_pos: Option<f64>,
    y_pos: Option<f64>,
    z_pos: Option<f64>,
    extrude: Option<f64>,
    feed: Option<f64>,
}

/// Parsed axis, feed-rate, and arc-centre offset values from a G2/G3 parameter
/// string.
struct ArcParams {
    x_pos: Option<f64>,
    y_pos: Option<f64>,
    z_pos: Option<f64>,
    extrude: Option<f64>,
    feed: Option<f64>,
    /// X offset from current position to arc centre.
    i_offset: Option<f64>,
    /// Y offset from current position to arc centre.
    j_offset: Option<f64>,
}

/// Parse the parameter string for G0, G1, and G92 commands.
///
/// Parameters may be space-separated or packed
/// (`OrcaSlicer` sometimes omits spaces: `X10.5Y20.0`).  Parsing is
/// case-insensitive for parameter letters.
///
/// # Errors
///
/// Returns [`ParseError::InvalidNumber`] if a parameter value string cannot be
/// parsed as `f64`.
fn parse_xyzef(params: &str, line_number: u32) -> Result<MoveParams, ParseError> {
    let mut result = MoveParams {
        x_pos: None,
        y_pos: None,
        z_pos: None,
        extrude: None,
        feed: None,
    };

    let mut cursor = 0usize;
    let bytes = params.as_bytes();

    while cursor < bytes.len() {
        // Skip whitespace between tokens.
        if bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
            continue;
        }

        // Parameter letter?
        let letter = bytes[cursor];
        if !letter.is_ascii_alphabetic() {
            // Unknown character in parameter string — skip it.
            cursor += 1;
            continue;
        }
        cursor += 1;

        // Collect the value: optional sign, digits, optional decimal point,
        // more digits.
        let value_start = cursor;
        if cursor < bytes.len() && (bytes[cursor] == b'+' || bytes[cursor] == b'-') {
            cursor += 1;
        }
        while cursor < bytes.len() && (bytes[cursor].is_ascii_digit() || bytes[cursor] == b'.') {
            cursor += 1;
        }
        let value_str = &params[value_start..cursor];

        if value_str.is_empty() || value_str == "+" || value_str == "-" {
            // Letter with no value — skip silently (e.g. bare `T` on M-codes).
            continue;
        }

        let value: f64 = value_str
            .parse()
            .map_err(|source| ParseError::InvalidNumber {
                line: line_number,
                value: value_str.to_owned(),
                source,
            })?;

        match letter.to_ascii_uppercase() {
            b'X' => result.x_pos = Some(value),
            b'Y' => result.y_pos = Some(value),
            b'Z' => result.z_pos = Some(value),
            b'E' => result.extrude = Some(value),
            b'F' => result.feed = Some(value),
            // Unrecognised parameter letter for these commands — ignore it.
            _ => {}
        }
    }

    Ok(result)
}

/// Parse the parameter string for G2 and G3 arc commands.
///
/// Extends [`parse_xyzef`] with `I` and `J` arc-centre offset parameters.
/// All parameters are optional; absent ones default to `None`.
///
/// # Errors
///
/// Returns [`ParseError::InvalidNumber`] if any parameter value string cannot
/// be parsed as `f64`.
fn parse_xyzef_ij(params: &str, line_number: u32) -> Result<ArcParams, ParseError> {
    let mut result = ArcParams {
        x_pos: None,
        y_pos: None,
        z_pos: None,
        extrude: None,
        feed: None,
        i_offset: None,
        j_offset: None,
    };

    let mut cursor = 0usize;
    let bytes = params.as_bytes();

    while cursor < bytes.len() {
        if bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
            continue;
        }

        let letter = bytes[cursor];
        if !letter.is_ascii_alphabetic() {
            cursor += 1;
            continue;
        }
        cursor += 1;

        let value_start = cursor;
        if cursor < bytes.len() && (bytes[cursor] == b'+' || bytes[cursor] == b'-') {
            cursor += 1;
        }
        while cursor < bytes.len() && (bytes[cursor].is_ascii_digit() || bytes[cursor] == b'.') {
            cursor += 1;
        }
        let value_str = &params[value_start..cursor];

        if value_str.is_empty() || value_str == "+" || value_str == "-" {
            continue;
        }

        let value: f64 = value_str
            .parse()
            .map_err(|source| ParseError::InvalidNumber {
                line: line_number,
                value: value_str.to_owned(),
                source,
            })?;

        match letter.to_ascii_uppercase() {
            b'X' => result.x_pos = Some(value),
            b'Y' => result.y_pos = Some(value),
            b'Z' => result.z_pos = Some(value),
            b'E' => result.extrude = Some(value),
            b'F' => result.feed = Some(value),
            b'I' => result.i_offset = Some(value),
            b'J' => result.j_offset = Some(value),
            _ => {}
        }
    }

    Ok(result)
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Convenience: parse a single line at line 1, offset 0.
    fn parse(s: &str) -> Result<GCodeCommand<'_>, ParseError> {
        parse_line(s, 1, 0).map(|s| s.inner)
    }

    // ── G2 / G3 — arc moves ──────────────────────────────────────────────────

    #[test]
    fn test_g2_cw_arc_with_ij_parses_to_arc_move_cw() {
        let cmd = parse("G2 X20 Y0 I10 J0").unwrap();
        assert_eq!(
            cmd,
            GCodeCommand::ArcMoveCW {
                x: Some(20.0),
                y: Some(0.0),
                z: None,
                e: None,
                f: None,
                i: Some(10.0),
                j: Some(0.0),
            }
        );
    }

    #[test]
    fn test_g3_ccw_arc_with_ij_parses_to_arc_move_ccw() {
        let cmd = parse("G3 X0 Y10 I0 J10").unwrap();
        assert_eq!(
            cmd,
            GCodeCommand::ArcMoveCCW {
                x: Some(0.0),
                y: Some(10.0),
                z: None,
                e: None,
                f: None,
                i: Some(0.0),
                j: Some(10.0),
            }
        );
    }

    #[test]
    fn test_g2_all_parameters_x_y_z_e_f_i_j() {
        let cmd = parse("G2 X10 Y10 Z0.2 E1.5 F3000 I5 J0").unwrap();
        assert_eq!(
            cmd,
            GCodeCommand::ArcMoveCW {
                x: Some(10.0),
                y: Some(10.0),
                z: Some(0.2),
                e: Some(1.5),
                f: Some(3000.0),
                i: Some(5.0),
                j: Some(0.0),
            }
        );
    }

    #[test]
    fn test_g2_missing_ij_defaults_to_none() {
        let cmd = parse("G2 X10 Y10").unwrap();
        assert_eq!(
            cmd,
            GCodeCommand::ArcMoveCW {
                x: Some(10.0),
                y: Some(10.0),
                z: None,
                e: None,
                f: None,
                i: None,
                j: None,
            }
        );
    }

    // ── G0 ────────────────────────────────────────────────────────────────────

    #[test]
    fn g0_rapid_move() {
        let cmd = parse("G0 X10 Y20 Z0.2 F3000").unwrap();
        assert_eq!(
            cmd,
            GCodeCommand::RapidMove {
                x: Some(10.0),
                y: Some(20.0),
                z: Some(0.2),
                f: Some(3000.0),
            }
        );
    }

    #[test]
    fn g00_leading_zero_is_rapid_move() {
        let cmd = parse("G00 X5").unwrap();
        assert!(matches!(cmd, GCodeCommand::RapidMove { x: Some(_), .. }));
    }

    // ── G1 ────────────────────────────────────────────────────────────────────

    #[test]
    fn g1_linear_move() {
        let cmd = parse("G1 X100.5 Y-50.3 E1.234 F1200").unwrap();
        assert_eq!(
            cmd,
            GCodeCommand::LinearMove {
                x: Some(100.5),
                y: Some(-50.3),
                z: None,
                e: Some(1.234),
                f: Some(1200.0),
            }
        );
    }

    #[test]
    fn g1_lowercase() {
        let cmd = parse("g1 x10 y20").unwrap();
        assert!(matches!(
            cmd,
            GCodeCommand::LinearMove {
                x: Some(_),
                y: Some(_),
                z: None,
                e: None,
                f: None
            }
        ));
    }

    #[test]
    fn g1_inline_comment_discarded() {
        // The command is what matters; the inline comment is slicer noise.
        let cmd = parse("G1 X10 Y20 ;move to start").unwrap();
        assert_eq!(
            cmd,
            GCodeCommand::LinearMove {
                x: Some(10.0),
                y: Some(20.0),
                z: None,
                e: None,
                f: None,
            }
        );
    }

    // ── G90 / G91 ─────────────────────────────────────────────────────────────

    #[test]
    fn g90_set_absolute() {
        assert_eq!(parse("G90").unwrap(), GCodeCommand::SetAbsolute);
    }

    #[test]
    fn g91_set_relative() {
        assert_eq!(parse("G91").unwrap(), GCodeCommand::SetRelative);
    }

    // ── G92 ───────────────────────────────────────────────────────────────────

    #[test]
    fn g92_reset_extruder() {
        let cmd = parse("G92 E0").unwrap();
        assert_eq!(
            cmd,
            GCodeCommand::SetPosition {
                x: None,
                y: None,
                z: None,
                e: Some(0.0),
            }
        );
    }

    // ── G28 ───────────────────────────────────────────────────────────────────

    #[test]
    fn g28_home_no_params() {
        let cmd = parse("G28").unwrap();
        assert_eq!(
            cmd,
            GCodeCommand::GCommand {
                code: 28,
                params: Cow::Borrowed(""),
            }
        );
    }

    #[test]
    fn g28_with_param() {
        let cmd = parse("G28 W").unwrap();
        assert_eq!(
            cmd,
            GCodeCommand::GCommand {
                code: 28,
                params: Cow::Borrowed("W"),
            }
        );
    }

    // ── M-codes ───────────────────────────────────────────────────────────────

    #[test]
    fn m104_set_temp() {
        let cmd = parse("M104 S200 T0").unwrap();
        assert_eq!(
            cmd,
            GCodeCommand::MetaCommand {
                code: 104,
                params: Cow::Borrowed("S200 T0"),
            }
        );
    }

    // ── Comments ──────────────────────────────────────────────────────────────

    #[test]
    fn comment_type_external_perimeter() {
        let cmd = parse(";TYPE:External perimeter").unwrap();
        assert_eq!(
            cmd,
            GCodeCommand::Comment {
                text: Cow::Borrowed("TYPE:External perimeter"),
            }
        );
    }

    #[test]
    fn comment_layer_change() {
        let cmd = parse(";LAYER_CHANGE").unwrap();
        assert_eq!(
            cmd,
            GCodeCommand::Comment {
                text: Cow::Borrowed("LAYER_CHANGE"),
            }
        );
    }

    #[test]
    fn comment_z_height() {
        let cmd = parse(";Z:0.20").unwrap();
        assert_eq!(
            cmd,
            GCodeCommand::Comment {
                text: Cow::Borrowed("Z:0.20"),
            }
        );
    }

    #[test]
    fn comment_with_leading_whitespace() {
        let cmd = parse("  ; indented comment").unwrap();
        assert_eq!(
            cmd,
            GCodeCommand::Comment {
                text: Cow::Borrowed(" indented comment"),
            }
        );
    }

    #[test]
    fn parenthesised_comment() {
        let cmd = parse("(this is a comment)").unwrap();
        assert_eq!(
            cmd,
            GCodeCommand::Comment {
                text: Cow::Borrowed("this is a comment"),
            }
        );
    }

    // ── Empty / whitespace ────────────────────────────────────────────────────

    #[test]
    fn empty_line() {
        let cmd = parse("").unwrap();
        assert_eq!(
            cmd,
            GCodeCommand::Unknown {
                raw: Cow::Borrowed("")
            }
        );
    }

    #[test]
    fn whitespace_only_line() {
        let cmd = parse("   ").unwrap();
        assert_eq!(
            cmd,
            GCodeCommand::Unknown {
                raw: Cow::Borrowed("")
            }
        );
    }

    // ── Unknown / Klipper macros ──────────────────────────────────────────────

    #[test]
    fn klipper_exclude_object() {
        let line = "EXCLUDE_OBJECT_DEFINE EXCLUDE_OBJECT_DEFINE name=obj_0";
        let cmd = parse(line).unwrap();
        assert_eq!(
            cmd,
            GCodeCommand::Unknown {
                raw: Cow::Borrowed(line),
            }
        );
    }

    // ── Error cases ───────────────────────────────────────────────────────────

    #[test]
    fn malformed_number_returns_error() {
        let err = parse("G1 X1.2.3").unwrap_err();
        assert!(matches!(err, ParseError::InvalidNumber { line: 1, .. }));
    }

    // ── Byte-offset and line-number tracking ──────────────────────────────────

    #[test]
    fn streaming_line_and_offset() {
        let src = "G90\nG91\n";
        let items: Vec<_> = parse_streaming(src).collect();
        assert_eq!(items.len(), 2);

        let first = items[0].as_ref().unwrap();
        assert_eq!(first.line, 1);
        assert_eq!(first.byte_offset, 0);

        let second = items[1].as_ref().unwrap();
        assert_eq!(second.line, 2);
        // "G90\n" is 4 bytes.
        assert_eq!(second.byte_offset, 4);
    }

    #[test]
    fn streaming_crlf_offsets() {
        // \r\n line endings: each line is 5 bytes.
        let src = "G90\r\nG91\r\n";
        let items: Vec<_> = parse_streaming(src).collect();
        assert_eq!(items.len(), 2);

        assert_eq!(items[0].as_ref().unwrap().byte_offset, 0);
        // "G90\r\n" is 5 bytes.
        assert_eq!(items[1].as_ref().unwrap().byte_offset, 5);
    }

    #[test]
    fn streaming_recovers_after_error() {
        let src = "G1 X1.2.3\nG90\n";
        let items: Vec<_> = parse_streaming(src).collect();
        assert_eq!(items.len(), 2);
        assert!(items[0].is_err());
        assert!(items[1].is_ok());
    }

    #[test]
    fn parse_all_stops_at_first_error() {
        let src = "G90\nG1 X1.2.3\nG91\n";
        let result = parse_all(src);
        assert!(result.is_err());
    }

    #[test]
    fn parse_all_success() {
        let src = "G90\nG91\n";
        let cmds = parse_all(src).unwrap();
        assert_eq!(cmds.len(), 2);
    }

    // ── OrcaSlicer packed params (no spaces) ─────────────────────────────────

    #[test]
    fn orcaslicer_packed_params() {
        // OrcaSlicer sometimes emits parameters with no spaces between them.
        let cmd = parse("G1 X10.5Y20.0E0.5F3000").unwrap();
        assert_eq!(
            cmd,
            GCodeCommand::LinearMove {
                x: Some(10.5),
                y: Some(20.0),
                z: None,
                e: Some(0.5),
                f: Some(3000.0),
            }
        );
    }

    // ── Spanned fields ────────────────────────────────────────────────────────

    #[test]
    fn spanned_fields_are_correct() {
        let spanned = parse_line("G90", 42, 1024).unwrap();
        assert_eq!(spanned.line, 42);
        assert_eq!(spanned.byte_offset, 1024);
        assert_eq!(spanned.inner, GCodeCommand::SetAbsolute);
        // Deref should give us the inner command transparently.
        assert_eq!(*spanned, GCodeCommand::SetAbsolute);
    }

    // ── File-final line without trailing newline ──────────────────────────────

    #[test]
    fn no_trailing_newline() {
        let src = "G90\nG91"; // no trailing '\n'
        let items: Vec<_> = parse_streaming(src).collect();
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].as_ref().unwrap().inner, GCodeCommand::SetRelative);
    }
}
