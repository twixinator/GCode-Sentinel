//! Built-in machine profiles and TOML-based profile loading.
//!
//! # Overview
//!
//! A [`MachineProfile`] captures the physical envelope and optional firmware
//! properties of a 3D printer.  Five profiles ship with the binary (embedded as
//! inline TOML strings so no filesystem access is required at runtime):
//!
//! | Name            | Bed (X × Y) | Z   | Notes                     |
//! |-----------------|-------------|-----|---------------------------|
//! | `ender3`        | 220 × 220   | 250 | Creality Ender-3 family   |
//! | `prusa_mk4`     | 250 × 210   | 220 | Prusa MK4                 |
//! | `voron_v2`      | 350 × 350   | 350 | Voron V2.4 (350 mm kit)   |
//! | `bambu_x1c`     | 256 × 256   | 256 | Bambu Lab X1 Carbon       |
//! | `generic_300`   | 300 × 300   | 400 | Generic 300 mm printer    |
//!
//! # Usage
//!
//! ```rust
//! use gcode_sentinel::machine_profile::load_profile;
//!
//! let profile = load_profile("ender3").expect("ender3 must be built-in");
//! assert_eq!(profile.max_x_mm, 220.0);
//! ```
//!
//! # Error handling
//!
//! [`load_profile`] returns [`ProfileError::UnknownProfile`] when the requested
//! name is not in the built-in registry.  The error message includes a
//! comma-separated list of every valid name so the user can self-correct.

use crate::models::MachineLimits;

// ─────────────────────────────────────────────────────────────────────────────
// Error type
// ─────────────────────────────────────────────────────────────────────────────

/// Errors that can occur when loading a machine profile.
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    /// The requested profile name is not in the built-in registry.
    ///
    /// `available` lists every valid profile name so the caller or user can
    /// correct the input without a separate lookup.
    #[error(
        "unknown machine profile '{name}'; available profiles: {available}",
        available = available.join(", ")
    )]
    UnknownProfile {
        /// The name that was requested but not found.
        name: String,
        /// Every valid built-in profile name, in registration order.
        available: Vec<String>,
    },

    /// A built-in profile string could not be parsed as TOML.
    ///
    /// This variant should never occur in a correctly assembled binary; its
    /// presence ensures that TOML parse failures surface clearly rather than
    /// panicking.
    #[error("failed to parse built-in profile '{name}': {source}")]
    ParseError {
        /// Name of the profile whose TOML could not be parsed.
        name: &'static str,
        /// Underlying TOML parse error.
        #[source]
        source: toml::de::Error,
    },
}

// ─────────────────────────────────────────────────────────────────────────────
// MachineProfile
// ─────────────────────────────────────────────────────────────────────────────

/// Physical envelope and optional firmware properties of a 3D printer.
///
/// All dimension fields are in millimetres.  Optional fields are absent when
/// the profile author does not constrain that property; the rest of the
/// pipeline treats `None` as "unconstrained".
///
/// # Examples
///
/// ```rust
/// use gcode_sentinel::machine_profile::load_profile;
///
/// let p = load_profile("bambu_x1c").unwrap();
/// assert_eq!(p.max_x_mm, 256.0);
/// assert_eq!(p.firmware.as_deref(), Some("bambu"));
/// ```
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct MachineProfile {
    /// Human-readable profile name (matches the lookup key).
    pub name: String,

    /// Maximum X-axis travel in millimetres.
    pub max_x_mm: f64,

    /// Maximum Y-axis travel in millimetres.
    pub max_y_mm: f64,

    /// Maximum Z-axis travel in millimetres.
    pub max_z_mm: f64,

    /// Maximum axis feedrate in mm/min, if the firmware enforces a limit.
    pub max_feedrate_mm_per_min: Option<f64>,

    /// Maximum axis acceleration in mm/s², if the firmware enforces a limit.
    pub max_acceleration_mm_per_s2: Option<f64>,

    /// Nozzle diameter in millimetres, if specified.
    pub nozzle_diameter_mm: Option<f64>,

    /// Firmware variant identifier (e.g. `"marlin"`, `"klipper"`, `"bambu"`).
    pub firmware: Option<String>,
}

impl MachineProfile {
    /// Converts this profile into [`MachineLimits`] using only the axis bounds.
    ///
    /// The optional fields (`max_feedrate_mm_per_min`, etc.) are not yet
    /// consumed by the analyser; they are preserved in the profile for future
    /// use.
    #[must_use]
    pub fn to_machine_limits(&self) -> MachineLimits {
        MachineLimits {
            max_x: self.max_x_mm,
            max_y: self.max_y_mm,
            max_z: self.max_z_mm,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Built-in profile registry
// ─────────────────────────────────────────────────────────────────────────────

/// A single entry in the built-in profile registry.
struct BuiltinEntry {
    /// The lookup key (lowercase, underscore-separated).
    key: &'static str,
    /// The TOML source for this profile.
    toml: &'static str,
}

/// All built-in profiles, in the order shown in the module-level table.
///
/// Profiles are stored as inline TOML strings rather than separate files so
/// that the binary is fully self-contained.  The TOML is parsed on first use;
/// parsing a dozen small strings is cheap enough that lazy initialisation is
/// not warranted.
static BUILTIN_PROFILES: &[BuiltinEntry] = &[
    BuiltinEntry {
        key: "ender3",
        toml: r#"
name = "ender3"
max_x_mm = 220.0
max_y_mm = 220.0
max_z_mm = 250.0
max_feedrate_mm_per_min = 500.0
nozzle_diameter_mm = 0.4
firmware = "marlin"
"#,
    },
    BuiltinEntry {
        key: "prusa_mk4",
        toml: r#"
name = "prusa_mk4"
max_x_mm = 250.0
max_y_mm = 210.0
max_z_mm = 220.0
max_feedrate_mm_per_min = 500.0
max_acceleration_mm_per_s2 = 1250.0
nozzle_diameter_mm = 0.4
firmware = "prusa"
"#,
    },
    BuiltinEntry {
        key: "voron_v2",
        toml: r#"
name = "voron_v2"
max_x_mm = 350.0
max_y_mm = 350.0
max_z_mm = 350.0
max_feedrate_mm_per_min = 600.0
max_acceleration_mm_per_s2 = 3000.0
nozzle_diameter_mm = 0.4
firmware = "klipper"
"#,
    },
    BuiltinEntry {
        key: "bambu_x1c",
        toml: r#"
name = "bambu_x1c"
max_x_mm = 256.0
max_y_mm = 256.0
max_z_mm = 256.0
max_feedrate_mm_per_min = 1200.0
max_acceleration_mm_per_s2 = 20000.0
nozzle_diameter_mm = 0.4
firmware = "bambu"
"#,
    },
    BuiltinEntry {
        key: "generic_300",
        toml: r#"
name = "generic_300"
max_x_mm = 300.0
max_y_mm = 300.0
max_z_mm = 400.0
"#,
    },
];

// ─────────────────────────────────────────────────────────────────────────────
// Public API
// ─────────────────────────────────────────────────────────────────────────────

/// Returns the names of every built-in profile, in registration order.
///
/// Use this to present the user with valid choices when an unknown name is
/// supplied, or to iterate over all profiles for validation purposes.
#[must_use]
pub fn available_profiles() -> Vec<&'static str> {
    BUILTIN_PROFILES.iter().map(|e| e.key).collect()
}

/// Loads a built-in machine profile by name.
///
/// The lookup is case-sensitive; names use lowercase with underscores
/// (e.g. `"ender3"`, `"prusa_mk4"`).
///
/// # Errors
///
/// - [`ProfileError::UnknownProfile`] — `name` is not in the built-in registry.
///   The error message lists all valid names.
/// - [`ProfileError::ParseError`] — the built-in TOML for the matched profile
///   is syntactically invalid.  This should never occur in a correctly built
///   binary; it is included for completeness.
pub fn load_profile(name: &str) -> Result<MachineProfile, ProfileError> {
    let entry = BUILTIN_PROFILES.iter().find(|e| e.key == name);

    let Some(entry) = entry else {
        let available = available_profiles().into_iter().map(String::from).collect();
        return Err(ProfileError::UnknownProfile {
            name: name.to_owned(),
            available,
        });
    };

    toml::from_str(entry.toml).map_err(|source| ProfileError::ParseError {
        name: entry.key,
        source,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── available_profiles ───────────────────────────────────────────────────

    #[test]
    fn test_available_profiles_returns_all_names() {
        let names = available_profiles();
        assert_eq!(
            names,
            vec![
                "ender3",
                "prusa_mk4",
                "voron_v2",
                "bambu_x1c",
                "generic_300"
            ]
        );
    }

    // ── load_profile: happy path for each built-in ───────────────────────────

    #[test]
    fn test_load_profile_ender3_returns_correct_limits() {
        let p = load_profile("ender3").expect("ender3 must load");
        assert_eq!(p.name, "ender3");
        assert_eq!(p.max_x_mm, 220.0);
        assert_eq!(p.max_y_mm, 220.0);
        assert_eq!(p.max_z_mm, 250.0);
        assert_eq!(p.max_feedrate_mm_per_min, Some(500.0));
        assert_eq!(p.nozzle_diameter_mm, Some(0.4));
        assert_eq!(p.firmware.as_deref(), Some("marlin"));
        assert_eq!(p.max_acceleration_mm_per_s2, None);
    }

    #[test]
    fn test_load_profile_prusa_mk4_returns_correct_limits() {
        let p = load_profile("prusa_mk4").expect("prusa_mk4 must load");
        assert_eq!(p.name, "prusa_mk4");
        assert_eq!(p.max_x_mm, 250.0);
        assert_eq!(p.max_y_mm, 210.0);
        assert_eq!(p.max_z_mm, 220.0);
        assert_eq!(p.max_acceleration_mm_per_s2, Some(1250.0));
        assert_eq!(p.firmware.as_deref(), Some("prusa"));
    }

    #[test]
    fn test_load_profile_voron_v2_returns_correct_limits() {
        let p = load_profile("voron_v2").expect("voron_v2 must load");
        assert_eq!(p.name, "voron_v2");
        assert_eq!(p.max_x_mm, 350.0);
        assert_eq!(p.max_y_mm, 350.0);
        assert_eq!(p.max_z_mm, 350.0);
        assert_eq!(p.max_acceleration_mm_per_s2, Some(3000.0));
        assert_eq!(p.firmware.as_deref(), Some("klipper"));
    }

    #[test]
    fn test_load_profile_bambu_x1c_returns_correct_limits() {
        let p = load_profile("bambu_x1c").expect("bambu_x1c must load");
        assert_eq!(p.name, "bambu_x1c");
        assert_eq!(p.max_x_mm, 256.0);
        assert_eq!(p.max_y_mm, 256.0);
        assert_eq!(p.max_z_mm, 256.0);
        assert_eq!(p.max_feedrate_mm_per_min, Some(1200.0));
        assert_eq!(p.max_acceleration_mm_per_s2, Some(20_000.0));
        assert_eq!(p.firmware.as_deref(), Some("bambu"));
    }

    #[test]
    fn test_load_profile_generic_300_returns_correct_limits() {
        let p = load_profile("generic_300").expect("generic_300 must load");
        assert_eq!(p.name, "generic_300");
        assert_eq!(p.max_x_mm, 300.0);
        assert_eq!(p.max_y_mm, 300.0);
        assert_eq!(p.max_z_mm, 400.0);
        assert_eq!(p.max_feedrate_mm_per_min, None);
        assert_eq!(p.max_acceleration_mm_per_s2, None);
        assert_eq!(p.nozzle_diameter_mm, None);
        assert_eq!(p.firmware, None);
    }

    // ── load_profile: unknown name ───────────────────────────────────────────

    #[test]
    fn test_load_profile_unknown_name_returns_error() {
        let result = load_profile("does_not_exist");
        assert!(result.is_err());
        assert!(matches!(result, Err(ProfileError::UnknownProfile { .. })));
    }

    #[test]
    fn test_load_profile_unknown_name_error_contains_all_profile_names() {
        let err = load_profile("nope").unwrap_err();
        let msg = err.to_string();
        // The error message must list every valid profile so the user can self-correct.
        assert!(msg.contains("ender3"), "error should mention ender3: {msg}");
        assert!(
            msg.contains("prusa_mk4"),
            "error should mention prusa_mk4: {msg}"
        );
        assert!(
            msg.contains("voron_v2"),
            "error should mention voron_v2: {msg}"
        );
        assert!(
            msg.contains("bambu_x1c"),
            "error should mention bambu_x1c: {msg}"
        );
        assert!(
            msg.contains("generic_300"),
            "error should mention generic_300: {msg}"
        );
        assert!(
            msg.contains("nope"),
            "error should echo the bad name: {msg}"
        );
    }

    #[test]
    fn test_load_profile_case_sensitive_uppercase_returns_error() {
        // Profile names are lowercase; uppercase must not silently match.
        let result = load_profile("Ender3");
        assert!(
            matches!(result, Err(ProfileError::UnknownProfile { .. })),
            "lookup must be case-sensitive"
        );
    }

    // ── to_machine_limits ────────────────────────────────────────────────────

    #[test]
    fn test_machine_profile_to_machine_limits_uses_axis_bounds() {
        let p = load_profile("ender3").expect("ender3 must load");
        let limits = p.to_machine_limits();
        assert_eq!(limits.max_x, 220.0);
        assert_eq!(limits.max_y, 220.0);
        assert_eq!(limits.max_z, 250.0);
    }

    #[test]
    fn test_machine_profile_to_machine_limits_voron_v2() {
        let p = load_profile("voron_v2").expect("voron_v2 must load");
        let limits = p.to_machine_limits();
        assert_eq!(limits.max_x, 350.0);
        assert_eq!(limits.max_y, 350.0);
        assert_eq!(limits.max_z, 350.0);
    }
}
