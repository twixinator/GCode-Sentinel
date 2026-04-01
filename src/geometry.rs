//! Shared arc geometry utilities.

use std::f64::consts::TAU;

/// Compute the angular span of an arc.
///
/// Returns the arc's angular span in radians — always in `(0, TAU]`.
/// A full circle (start == end, or sub-epsilon delta after direction
/// normalisation) returns `TAU`.
///
/// # Arguments
///
/// * `start_angle` – starting angle in radians.
/// * `end_angle`   – ending angle in radians.
/// * `clockwise`   – `true` for a CW (G2) arc, `false` for CCW (G3).
///
/// # Notes
///
/// Uses `d <= 0.0` as the wrap condition (`arc_fitter` convention): when
/// `start == end` the raw delta is exactly 0.0, which is caught here and
/// produces `TAU` before the sub-epsilon guard is even reached. The
/// sub-epsilon guard (`delta < 1e-10`) handles the remaining near-zero cases
/// that floating-point arithmetic can produce.
#[must_use]
pub(crate) fn arc_span(start_angle: f64, end_angle: f64, clockwise: bool) -> f64 {
    let delta = if clockwise {
        // CW: angle decreases from start to end.
        let d = start_angle - end_angle;
        if d <= 0.0 {
            d + TAU
        } else {
            d
        }
    } else {
        // CCW: angle increases from start to end.
        let d = end_angle - start_angle;
        if d <= 0.0 {
            d + TAU
        } else {
            d
        }
    };
    // Exact coincidence of start and end, or any sub-epsilon residue, means a
    // full circle.
    if delta < 1e-10 {
        TAU
    } else {
        delta
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── Boundary behaviour (moved from arc_fitter after dedup) ──────────────

    #[test]
    fn test_arc_span_exact_zero_delta_cw_returns_tau() {
        // CW arc where start == end: the span must be a full circle (TAU), not zero.
        let angle = std::f64::consts::FRAC_PI_2;
        assert_eq!(arc_span(angle, angle, true), TAU);
    }

    #[test]
    fn test_arc_span_exact_zero_delta_ccw_returns_tau() {
        // CCW arc where start == end: same full-circle expectation.
        let angle = std::f64::consts::FRAC_PI_2;
        assert_eq!(arc_span(angle, angle, false), TAU);
    }

    #[test]
    fn test_arc_span_near_zero_delta_below_epsilon_returns_tau() {
        // delta just below 1e-10 after direction normalisation also means full circle.
        // For a CCW arc: end - start = 5e-11 < 1e-10 → TAU.
        let start = 0.0_f64;
        let end = 5e-11_f64;
        assert_eq!(arc_span(start, end, false), TAU);
    }

    // ─── Basic span calculations ──────────────────────────────────────────────

    #[test]
    fn test_arc_span_quarter_circle_cw_returns_pi_over_2() {
        // CW quarter arc: π/2 → 0.
        let result = arc_span(std::f64::consts::FRAC_PI_2, 0.0, true);
        assert!((result - std::f64::consts::FRAC_PI_2).abs() < 1e-10);
    }

    #[test]
    fn test_arc_span_half_circle_ccw_returns_pi() {
        // CCW half arc: 0 → π.
        let result = arc_span(0.0, std::f64::consts::PI, false);
        assert!((result - std::f64::consts::PI).abs() < 1e-10);
    }

    // ─── M4: wrap-around and full-circle arithmetic ───────────────────────────

    #[test]
    fn test_arc_span_cw_crossing_zero_returns_correct_radians() {
        // CW arc from 10° to 350°: sweeps through 0°, span = 20°.
        let start = 10.0_f64.to_radians();
        let end = 350.0_f64.to_radians();
        let expected = 20.0_f64.to_radians();
        assert!((arc_span(start, end, true) - expected).abs() < 1e-10);
    }

    #[test]
    fn test_arc_span_ccw_crossing_zero_returns_correct_radians() {
        // CCW arc from 350° to 10°: sweeps through 0°, span = 20°.
        let start = 350.0_f64.to_radians();
        let end = 10.0_f64.to_radians();
        let expected = 20.0_f64.to_radians();
        assert!((arc_span(start, end, false) - expected).abs() < 1e-10);
    }

    #[test]
    fn test_arc_span_full_circle_cw_returns_tau() {
        // CW full circle: start == end → TAU.
        let angle = std::f64::consts::PI;
        assert_eq!(arc_span(angle, angle, true), TAU);
    }

    #[test]
    fn test_arc_span_full_circle_ccw_returns_tau() {
        // CCW full circle: start == end → TAU.
        let angle = std::f64::consts::PI;
        assert_eq!(arc_span(angle, angle, false), TAU);
    }

    #[test]
    fn test_arc_span_standard_quarter_ccw_returns_half_pi() {
        // CCW quarter arc: 0° → 90°, span = π/2.
        let result = arc_span(0.0, std::f64::consts::FRAC_PI_2, false);
        assert!((result - std::f64::consts::FRAC_PI_2).abs() < 1e-10);
    }
}
