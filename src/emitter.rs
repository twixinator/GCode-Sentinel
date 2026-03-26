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
