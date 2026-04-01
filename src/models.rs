//! Core data types for GCode-Sentinel.
//!
//! This module defines the abstract syntax tree (AST) for G-Code commands,
//! source-location wrappers, geometry primitives, machine configuration, and
//! parse error types.
//!
//! # Lifetime parameter `'a`
//!
//! [`GCodeCommand`] carries a lifetime `'a` that ties verbatim text fields
//! (`Comment`, `Unknown`, `MetaCommand` params) back to the source buffer.
//! When the source is memory-mapped those fields are zero-copy `&str` slices;
//! when commands are synthesised by the optimiser they hold owned `String`
//! values.  [`std::borrow::Cow`] provides both without changing the API.
//!
//! Numeric fields (`f64`) carry no lifetime — the parser converts them up
//! front, freeing the optimiser to create new move commands without any
//! reference to the original source buffer.

use std::borrow::Cow;
use std::fmt;
use std::ops::Deref;

// ──────────────────────────────────────────────────────────────────────────────
// GCodeCommand
// ──────────────────────────────────────────────────────────────────────────────

/// A single parsed G-Code command.
///
/// Numeric parameters are stored as [`f64`] values parsed at read time.  Only
/// fields that must preserve the original source text (comments, unknown lines,
/// M-command parameter strings) use [`Cow<'a, str>`] to remain zero-copy over a
/// memory-mapped buffer while still allowing the optimiser to synthesise new
/// commands with owned strings.
#[derive(Debug, Clone, PartialEq)]
pub enum GCodeCommand<'a> {
    /// `G0` — rapid (non-extruding) move to the given coordinates.
    ///
    /// Any subset of axes may be present; absent axes are left unchanged.
    RapidMove {
        /// Target X coordinate in millimetres, if specified.
        x: Option<f64>,
        /// Target Y coordinate in millimetres, if specified.
        y: Option<f64>,
        /// Target Z coordinate in millimetres, if specified.
        z: Option<f64>,
        /// Feed-rate in mm/min, if specified.
        f: Option<f64>,
    },

    /// `G1` — linear move with optional extrusion.
    ///
    /// Any subset of axes and the extruder axis may be present.
    LinearMove {
        /// Target X coordinate in millimetres, if specified.
        x: Option<f64>,
        /// Target Y coordinate in millimetres, if specified.
        y: Option<f64>,
        /// Target Z coordinate in millimetres, if specified.
        z: Option<f64>,
        /// Extruder axis position/delta in millimetres, if specified.
        e: Option<f64>,
        /// Feed-rate in mm/min, if specified.
        f: Option<f64>,
    },

    /// `G2` — clockwise arc move.
    ///
    /// Centre offset (I, J) is relative to the arc start point.
    /// Any subset of target axes may be present.
    ArcMoveCW {
        /// Target X coordinate in millimetres, if specified.
        x: Option<f64>,
        /// Target Y coordinate in millimetres, if specified.
        y: Option<f64>,
        /// Target Z coordinate in millimetres, if specified.
        z: Option<f64>,
        /// Extruder axis position/delta in millimetres, if specified.
        e: Option<f64>,
        /// Feed-rate in mm/min, if specified.
        f: Option<f64>,
        /// X offset from current position to arc centre, if specified.
        i: Option<f64>,
        /// Y offset from current position to arc centre, if specified.
        j: Option<f64>,
    },

    /// `G3` — counter-clockwise arc move.
    ///
    /// Centre offset (I, J) is relative to the arc start point.
    /// Any subset of target axes may be present.
    ArcMoveCCW {
        /// Target X coordinate in millimetres, if specified.
        x: Option<f64>,
        /// Target Y coordinate in millimetres, if specified.
        y: Option<f64>,
        /// Target Z coordinate in millimetres, if specified.
        z: Option<f64>,
        /// Extruder axis position/delta in millimetres, if specified.
        e: Option<f64>,
        /// Feed-rate in mm/min, if specified.
        f: Option<f64>,
        /// X offset from current position to arc centre, if specified.
        i: Option<f64>,
        /// Y offset from current position to arc centre, if specified.
        j: Option<f64>,
    },

    /// `G90` — set positioning to absolute mode.
    SetAbsolute,

    /// `G91` — set positioning to relative mode.
    SetRelative,

    /// `G92` — set the current logical position without moving the head.
    ///
    /// Commonly used to reset the extruder counter (`G92 E0`).
    SetPosition {
        /// Override X logical position, if specified.
        x: Option<f64>,
        /// Override Y logical position, if specified.
        y: Option<f64>,
        /// Override Z logical position, if specified.
        z: Option<f64>,
        /// Override extruder logical position, if specified.
        e: Option<f64>,
    },

    /// An M-code command whose parameter string is preserved verbatim.
    ///
    /// The raw parameter text (everything after `M<code>`) is kept as a
    /// [`Cow`] so that round-tripping through the emitter is lossless even for
    /// M-codes the parser does not specifically recognise.
    MetaCommand {
        /// The numeric M-code (e.g. `104` for `M104`).
        code: u16,
        /// The raw parameter string following the M-code, trimmed of leading
        /// whitespace.  May be empty for bare M-codes such as `M84`.
        params: Cow<'a, str>,
    },

    /// A comment line beginning with `;` or enclosed in `( ... )`.
    Comment {
        /// The comment text, excluding the delimiter characters.
        text: Cow<'a, str>,
    },

    /// Any G-code not specifically recognised by the parser (e.g. G2, G3, G4,
    /// G28, G29).
    ///
    /// Preserved with its raw parameter string so the emitter can round-trip
    /// the command without loss.
    GCommand {
        /// The numeric G-code (e.g. `28` for `G28`).
        code: u16,
        /// The raw parameter string following the G-code, trimmed of leading
        /// whitespace.  May be empty for bare codes such as `G28`.
        params: Cow<'a, str>,
    },

    /// An unrecognised line preserved verbatim for lossless round-tripping.
    ///
    /// Used for lines that are not recognisable as any G/M-code form at all
    /// (e.g. Klipper macro calls, `EXCLUDE_OBJECT_DEFINE`, blank lines with
    /// only whitespace, `OrcaSlicer` thumbnail data).
    Unknown {
        /// The raw source line exactly as it appeared in the input.
        raw: Cow<'a, str>,
    },
}

// ──────────────────────────────────────────────────────────────────────────────
// Spanned<T>
// ──────────────────────────────────────────────────────────────────────────────

/// Wraps any value with its source location (line number and byte offset).
///
/// Every command produced by the parser is wrapped in `Spanned` so that
/// diagnostics, error messages, and optimiser change-logs can always reference
/// the exact position in the input file.
///
/// # Dereferencing
///
/// `Spanned<T>` implements [`Deref<Target = T>`][Deref], so you can use it
/// transparently wherever a `&T` is expected without extracting `.inner`
/// manually.
#[derive(Debug, Clone, PartialEq)]
pub struct Spanned<T> {
    /// The wrapped value.
    pub inner: T,

    /// 1-based line number of this command in the source file.
    ///
    /// Using `u32` supports files up to ~4 billion lines, which is far beyond
    /// any realistic G-Code file.
    pub line: u32,

    /// Byte offset from the start of the source buffer to the first byte of
    /// this command's line.
    ///
    /// `u64` is used so that the type is valid for files larger than 4 GiB on
    /// 32-bit platforms, consistent with memory-mapped I/O on large files.
    pub byte_offset: u64,
}

impl<T> Deref for Spanned<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Point3D
// ──────────────────────────────────────────────────────────────────────────────

/// A point in three-dimensional Cartesian space (X, Y, Z) in millimetres.
///
/// Used to track the current print-head position and to represent bounding-box
/// corners in analysis results.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Point3D {
    /// X axis position in millimetres.
    pub x: f64,
    /// Y axis position in millimetres.
    pub y: f64,
    /// Z axis position in millimetres.
    pub z: f64,
}

impl Default for Point3D {
    /// Returns the origin `(0.0, 0.0, 0.0)`.
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        }
    }
}

impl fmt::Display for Point3D {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {}, {})", self.x, self.y, self.z)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// MachineLimits
// ──────────────────────────────────────────────────────────────────────────────

/// Physical travel limits of a 3D printer in millimetres.
///
/// The analyser uses these bounds to detect moves that would drive the print
/// head outside the printable volume.  The defaults correspond to a common
/// 300 x 300 x 400 mm printer such as the Creality Ender-5 Pro.
#[derive(Debug, Clone, PartialEq)]
pub struct MachineLimits {
    /// Maximum X axis travel in millimetres.
    pub max_x: f64,
    /// Maximum Y axis travel in millimetres.
    pub max_y: f64,
    /// Maximum Z axis travel in millimetres.
    pub max_z: f64,
}

impl Default for MachineLimits {
    /// Returns limits for a common 300 x 300 x 400 mm printer.
    fn default() -> Self {
        Self {
            max_x: 300.0,
            max_y: 300.0,
            max_z: 400.0,
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// ParseError
// ──────────────────────────────────────────────────────────────────────────────

/// Errors that can occur while parsing a G-Code source file.
///
/// Every variant carries a `line` field so that the error message can point the
/// user directly to the offending line.  The parser is designed to be
/// *recoverable*: each `parse_streaming` item is a `Result`, so a single bad
/// line does not abort parsing of the rest of the file.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// A line could not be interpreted as any known or unknown G-Code token.
    #[error("invalid G-Code on line {line}: {message}")]
    InvalidLine {
        /// 1-based line number of the offending line.
        line: u32,
        /// Human-readable description of what was wrong.
        message: String,
    },

    /// A parameter that should be a floating-point number could not be parsed.
    #[error("invalid number '{value}' on line {line}: {source}")]
    InvalidNumber {
        /// 1-based line number of the offending line.
        line: u32,
        /// The exact string token that failed to parse.
        value: String,
        /// The underlying [`std::num::ParseFloatError`].
        #[source]
        source: std::num::ParseFloatError,
    },

    /// The input ended in the middle of a construct that required more tokens.
    #[error("unexpected end of input on line {line}")]
    UnexpectedEof {
        /// 1-based line number where the input was exhausted.
        line: u32,
    },
}
