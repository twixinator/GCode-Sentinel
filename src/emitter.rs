//! Serialises a [`GCodeCommand`] AST back to G-Code text.
//!
//! The emitter is the symmetric counterpart of the parser: it converts the
//! in-memory representation back into a text form suitable for writing to a
//! file or piping to firmware.
//!
//! [`GCodeCommand`]: crate::models::GCodeCommand

use std::io;
use std::io::Write;

use crate::models::{GCodeCommand, Spanned};

// ──────────────────────────────────────────────────────────────────────────────
// EmitConfig
// ──────────────────────────────────────────────────────────────────────────────

/// Configuration for the emitter.
#[derive(Debug, Clone)]
pub struct EmitConfig {
    /// Number of decimal places to use for coordinate values.
    ///
    /// G-Code firmware typically expects 3–4 decimal places.  Defaults to 4.
    pub decimal_places: usize,

    /// Line ending sequence: `"\n"` (Unix) or `"\r\n"` (Windows/Marlin).
    ///
    /// Defaults to `"\n"`.
    pub line_ending: &'static str,
}

impl Default for EmitConfig {
    fn default() -> Self {
        Self {
            decimal_places: 4,
            line_ending: "\n",
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// EmitError
// ──────────────────────────────────────────────────────────────────────────────

/// Errors that can occur while emitting G-Code.
#[derive(Debug, thiserror::Error)]
pub enum EmitError {
    /// An I/O error occurred while writing to the output.
    #[error("I/O error while emitting G-Code: {0}")]
    Io(#[from] io::Error),
}

// ──────────────────────────────────────────────────────────────────────────────
// emit
// ──────────────────────────────────────────────────────────────────────────────

/// Serialises a slice of spanned G-Code commands to the given writer.
///
/// # Errors
///
/// Returns [`EmitError::Io`] if any write to `writer` fails.
pub fn emit<W>(
    commands: &[Spanned<GCodeCommand<'_>>],
    writer: &mut W,
    config: &EmitConfig,
) -> Result<(), EmitError>
where
    W: Write,
{
    for spanned in commands {
        emit_command(&spanned.inner, writer, config)?;
        writer.write_all(config.line_ending.as_bytes())?;
    }
    Ok(())
}

/// Serialises a single [`GCodeCommand`] to the given writer **without** a
/// trailing newline.
///
/// # Errors
///
/// Returns [`EmitError::Io`] if any write fails.
pub fn emit_command<W>(
    command: &GCodeCommand<'_>,
    writer: &mut W,
    config: &EmitConfig,
) -> Result<(), EmitError>
where
    W: Write,
{
    let dp = config.decimal_places;
    match command {
        GCodeCommand::RapidMove { x, y, z, f } => {
            write!(writer, "G0")?;
            emit_opt_coord(writer, 'X', *x, dp)?;
            emit_opt_coord(writer, 'Y', *y, dp)?;
            emit_opt_coord(writer, 'Z', *z, dp)?;
            emit_opt_coord(writer, 'F', *f, dp)?;
        }
        GCodeCommand::LinearMove { x, y, z, e, f } => {
            write!(writer, "G1")?;
            emit_opt_coord(writer, 'X', *x, dp)?;
            emit_opt_coord(writer, 'Y', *y, dp)?;
            emit_opt_coord(writer, 'Z', *z, dp)?;
            emit_opt_coord(writer, 'E', *e, dp)?;
            emit_opt_coord(writer, 'F', *f, dp)?;
        }
        GCodeCommand::ArcMoveCW {
            x,
            y,
            z,
            e,
            f,
            i,
            j,
        } => {
            write!(writer, "G2")?;
            emit_opt_coord(writer, 'X', *x, dp)?;
            emit_opt_coord(writer, 'Y', *y, dp)?;
            emit_opt_coord(writer, 'Z', *z, dp)?;
            emit_opt_coord(writer, 'E', *e, dp)?;
            emit_opt_coord(writer, 'F', *f, dp)?;
            emit_opt_coord(writer, 'I', *i, dp)?;
            emit_opt_coord(writer, 'J', *j, dp)?;
        }
        GCodeCommand::ArcMoveCCW {
            x,
            y,
            z,
            e,
            f,
            i,
            j,
        } => {
            write!(writer, "G3")?;
            emit_opt_coord(writer, 'X', *x, dp)?;
            emit_opt_coord(writer, 'Y', *y, dp)?;
            emit_opt_coord(writer, 'Z', *z, dp)?;
            emit_opt_coord(writer, 'E', *e, dp)?;
            emit_opt_coord(writer, 'F', *f, dp)?;
            emit_opt_coord(writer, 'I', *i, dp)?;
            emit_opt_coord(writer, 'J', *j, dp)?;
        }
        GCodeCommand::SetAbsolute => write!(writer, "G90")?,
        GCodeCommand::SetRelative => write!(writer, "G91")?,
        GCodeCommand::SetPosition { x, y, z, e } => {
            write!(writer, "G92")?;
            emit_opt_coord(writer, 'X', *x, dp)?;
            emit_opt_coord(writer, 'Y', *y, dp)?;
            emit_opt_coord(writer, 'Z', *z, dp)?;
            emit_opt_coord(writer, 'E', *e, dp)?;
        }
        GCodeCommand::GCommand { code, params } => {
            if params.is_empty() {
                write!(writer, "G{code}")?;
            } else {
                write!(writer, "G{code} {params}")?;
            }
        }
        GCodeCommand::MetaCommand { code, params } => {
            if params.is_empty() {
                write!(writer, "M{code}")?;
            } else {
                write!(writer, "M{code} {params}")?;
            }
        }
        GCodeCommand::Comment { text } => write!(writer, ";{text}")?,
        GCodeCommand::Unknown { raw } => write!(writer, "{raw}")?,
    }
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────────────

fn emit_opt_coord<W: Write>(
    writer: &mut W,
    axis: char,
    value: Option<f64>,
    decimal_places: usize,
) -> Result<(), EmitError> {
    if let Some(v) = value {
        write!(writer, " {axis}{v:.decimal_places$}")?;
    }
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::GCodeCommand;

    fn emit_to_string(cmd: &GCodeCommand<'_>, dp: usize) -> String {
        let config = EmitConfig {
            decimal_places: dp,
            line_ending: "\n",
        };
        let mut buf = Vec::new();
        emit_command(cmd, &mut buf, &config).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn test_emit_arc_move_cw_produces_g2_with_correct_params() {
        let cmd = GCodeCommand::ArcMoveCW {
            x: Some(20.0),
            y: Some(0.0),
            z: None,
            e: None,
            f: Some(3000.0),
            i: Some(10.0),
            j: Some(0.0),
        };
        let s = emit_to_string(&cmd, 4);
        assert!(s.starts_with("G2"), "expected G2 prefix, got: {s}");
        assert!(s.contains("X20.0000"), "expected X20.0000, got: {s}");
        assert!(s.contains("Y0.0000"), "expected Y0.0000, got: {s}");
        assert!(s.contains("F3000.0000"), "expected F3000.0000, got: {s}");
        assert!(s.contains("I10.0000"), "expected I10.0000, got: {s}");
        assert!(s.contains("J0.0000"), "expected J0.0000, got: {s}");
        assert!(!s.contains('Z'), "unexpected Z in: {s}");
        assert!(!s.contains('E'), "unexpected E in: {s}");
    }

    #[test]
    fn test_emit_arc_move_ccw_produces_g3_with_correct_params() {
        let cmd = GCodeCommand::ArcMoveCCW {
            x: Some(0.0),
            y: Some(10.0),
            z: None,
            e: None,
            f: None,
            i: Some(0.0),
            j: Some(10.0),
        };
        let s = emit_to_string(&cmd, 4);
        assert!(s.starts_with("G3"), "expected G3 prefix, got: {s}");
        assert!(s.contains("X0.0000"), "expected X0.0000, got: {s}");
        assert!(s.contains("Y10.0000"), "expected Y10.0000, got: {s}");
        assert!(s.contains("I0.0000"), "expected I0.0000, got: {s}");
        assert!(s.contains("J10.0000"), "expected J10.0000, got: {s}");
    }

    #[test]
    fn test_emit_arc_respects_decimal_places() {
        let cmd = GCodeCommand::ArcMoveCW {
            x: Some(1.0),
            y: Some(2.0),
            z: None,
            e: None,
            f: None,
            i: Some(3.0),
            j: Some(4.0),
        };
        let s2 = emit_to_string(&cmd, 2);
        assert!(s2.contains("X1.00"), "expected X1.00, got: {s2}");
        assert!(s2.contains("I3.00"), "expected I3.00, got: {s2}");

        let s0 = emit_to_string(&cmd, 0);
        assert!(s0.contains("X1"), "expected X1 (no decimals), got: {s0}");
        assert!(s0.contains("I3"), "expected I3 (no decimals), got: {s0}");
    }

    #[test]
    fn test_emit_arc_omits_absent_optional_params() {
        let cmd = GCodeCommand::ArcMoveCW {
            x: Some(5.0),
            y: None,
            z: None,
            e: None,
            f: None,
            i: Some(2.5),
            j: None,
        };
        let s = emit_to_string(&cmd, 3);
        assert!(!s.contains('Y'), "Y should be absent, got: {s}");
        assert!(!s.contains('Z'), "Z should be absent, got: {s}");
        assert!(!s.contains('E'), "E should be absent, got: {s}");
        assert!(!s.contains('F'), "F should be absent, got: {s}");
        assert!(!s.contains('J'), "J should be absent, got: {s}");
        assert!(s.contains("X5.000"), "X should be present, got: {s}");
        assert!(s.contains("I2.500"), "I should be present, got: {s}");
    }
}
