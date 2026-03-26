# GCode-Sentinel Optimization Roadmap

**Version:** 2.0 Research Document
**Date:** 2026-03-26
**Status:** Research & Planning -- all techniques described here are candidates for v2+
**Audience:** Contributors, maintainers, and downstream integrators

---

## Table of Contents

1. [Preamble: Scope and Safety Philosophy](#1-preamble-scope-and-safety-philosophy)
2. [Travel Move Optimization](#2-travel-move-optimization)
3. [Arc Fitting (G2/G3 Replacement)](#3-arc-fitting-g2g3-replacement)
4. [Retraction Optimization](#4-retraction-optimization)
5. [Speed/Feed Rate Optimization](#5-speedfeed-rate-optimization)
6. [Redundant Move Elimination (Beyond v1)](#6-redundant-move-elimination-beyond-v1)
7. [Temperature Tower / Calibration Detection](#7-temperature-tower--calibration-detection)
8. [Layer Time Normalization](#8-layer-time-normalization)
9. [Support Structure Analysis](#9-support-structure-analysis)
10. [Estimated Difficulty and Priority Matrix](#10-estimated-difficulty-and-priority-matrix)
11. [G-Code Dialect Considerations](#11-g-code-dialect-considerations)
12. [Appendix A: AST Extension Requirements](#appendix-a-ast-extension-requirements)
13. [Appendix B: Testing Strategy for Optimizations](#appendix-b-testing-strategy-for-optimizations)
14. [Appendix C: References and Prior Art](#appendix-c-references-and-prior-art)

---

## 1. Preamble: Scope and Safety Philosophy

### What v1 Implements

GCode-Sentinel v1 provides two capabilities:

1. **Simulation / dry-run:** A virtual print head tracks position state through the entire G-Code file, computing bounding boxes, distances, filament usage, estimated print time, and out-of-bounds diagnostics.
2. **Redundant command removal:** Elimination of zero-delta moves -- commands that specify coordinates identical to the current print head state.

### What This Document Covers

Every optimization technique described below is a *candidate* for v2 and beyond. Each section provides enough detail for a developer to begin implementation, but the final selection and ordering will depend on user demand, contributor availability, and the evolving state of the codebase.

### Safety Philosophy

GCode-Sentinel modifies files that will control physical hardware. Every optimization technique carries risk proportional to its aggressiveness. The project adopts these non-negotiable principles:

1. **Opt-in only.** No optimization runs unless the user explicitly enables it. The `--check-only` flag is the safe default.
2. **Post-optimization validation.** After every optimization pass, the analyzer re-validates the output. If the optimized G-Code introduces new violations (out-of-bounds, missing homing, temperature anomalies), the tool aborts and reports the regression.
3. **Change transparency.** Every modification is logged as an `OptimizationChange` with the affected line number and a human-readable description. The `--dry-run` mode shows what *would* change without modifying any file.
4. **Conservative defaults.** Where a technique has tunable parameters (tolerance thresholds, distance limits), defaults are chosen to minimize risk of print failure, not to maximize theoretical improvement.
5. **Golden-file regression tests.** Every optimization must have known-input / expected-output test pairs. Any change to optimizer behavior that alters output must be caught by CI before merge.

### Current AST Capabilities

The existing `GCodeCommand` enum (in `src/models.rs`) already parses G0 and G1 into structured numeric fields. G2, G3, and other G-codes are captured as `GCommand { code, params }` with raw parameter strings. Several optimizations below will require extending the AST with first-class variants for additional commands. These requirements are collected in [Appendix A](#appendix-a-ast-extension-requirements).

---

## 2. Travel Move Optimization

### 2.1 Problem Statement

When a slicer generates G-Code, it processes perimeters, infill, and support structures in a largely arbitrary order within each layer. Between these "print islands" (disconnected regions on the same layer), the nozzle must travel without extruding. These travel moves can be long, crossing the entire bed, even when a shorter route exists.

Excessive travel increases print time and introduces stringing (ooze deposited along travel paths). On a typical multi-part print plate, travel moves can account for 5-15% of total print time.

### 2.2 Technique: Nearest-Neighbor Reordering

**Description:** After identifying all print islands on a given layer, reorder them so that each island's start point is the closest unvisited island start from the current nozzle position. This is the classic nearest-neighbor heuristic for the Travelling Salesman Problem (TSP).

**Algorithm:**
1. Parse a layer's G-Code and identify island boundaries. An island boundary is defined by a non-extruding travel move (G0) followed by extruding moves (G1 with E > 0).
2. For each island, record the entry point (first XY coordinate) and exit point (last XY coordinate).
3. Starting from the nozzle position at layer start, greedily select the nearest unvisited island entry point using Euclidean distance.
4. Re-emit the layer's G-Code in the new island order, updating travel moves between islands.

**Expected gains:**
- Print time reduction: 3-10% on multi-part plates or prints with many separate regions per layer.
- Stringing reduction: proportional to reduced travel distance.
- Single-part prints with simple geometry: negligible improvement.

**Implementation difficulty:** Medium.
- Requires reliable island detection (parsing travel vs. extrusion boundaries).
- Must preserve relative ordering *within* each island (perimeter before infill, outer wall before inner wall).
- The layer boundary detection logic from the analyzer can be reused.

**Risks:**
- **Medium risk.** Incorrect island boundary detection could split a single island into fragments, causing the nozzle to revisit the same region and deposit blobs. Mitigation: validate that total extrusion per layer is unchanged after reordering.
- Nearest-neighbor does not guarantee optimal ordering; it can produce pathological results on adversarial layouts. Acceptable because the improvement over random ordering is substantial and the heuristic runs in O(n^2) time which is practical for typical island counts (< 100 per layer).

### 2.3 Technique: TSP-Based Optimal Travel Sequencing

**Description:** Replace the nearest-neighbor heuristic with a more rigorous TSP solver for island ordering. For typical layer island counts (5-50), exact or near-optimal solutions are feasible.

**Algorithm options:**
- **2-opt / 3-opt local search:** Start with nearest-neighbor ordering, then iteratively improve by reversing segments. Simple to implement, runs in O(n^2) per improvement step, converges quickly for small n.
- **Or-opt:** Move single islands or pairs to better positions in the sequence. Less disruptive than 2-opt, often combined with it.
- **Concorde / LKH (external):** Gold-standard TSP solvers. Overkill for < 100 nodes but mentioned for completeness. Would require FFI or subprocess invocation. Not recommended for v2 due to dependency complexity.

**Expected gains over nearest-neighbor:** 5-20% further reduction in travel distance on complex layers. Diminishing returns on simple layouts.

**Implementation difficulty:** Medium-High.
- 2-opt is straightforward to implement; 3-opt and LKH are substantially more complex.
- Recommended approach: implement nearest-neighbor first (Section 2.2), then add 2-opt as an optional refinement step behind a `--travel-optimizer=2opt` flag.

**Risks:**
- **Low additional risk** beyond nearest-neighbor, since the island ordering is still a permutation of the same islands. Same validation applies: total extrusion unchanged, no islands lost or duplicated.

### 2.4 Technique: Combing (Travel Within Print Perimeter)

**Description:** Instead of travelling in a straight line between two points on the same printed region, route the nozzle along a path that stays inside already-printed perimeters. This eliminates the need for retraction during short intra-island travels and prevents nozzle ooze from landing on the exterior surface.

**Algorithm:**
1. Build a 2D polygon representation of the current layer's printed perimeters.
2. For each travel move, check if the straight-line path crosses an exterior perimeter.
3. If it does, compute an alternative path that stays within the polygon. Approaches:
   - **Visibility graph:** Compute shortest path through the polygon's visibility graph. Exact but O(n^2 log n) where n is polygon vertex count.
   - **Simple offset:** Hug the inner perimeter wall. Less optimal but much simpler.
   - **Convex hull shortcut:** If both start and end points are inside the same convex region, the straight line is safe.

**Expected gains:**
- Reduces visible surface defects (blobs, strings on outer walls).
- Reduces retraction count (travel within perimeter does not require retraction).
- Print time impact: slightly increases travel distance but eliminates retraction dwell time, often net-neutral or slightly faster.

**Implementation difficulty:** High.
- Requires building a 2D polygon model of each layer from the G-Code move sequence. This is a geometry problem, not just a parsing problem.
- Polygon construction from G-Code is fragile: perimeters are not labeled as such in all slicer outputs. Heuristics needed (closed loops of extrusion moves at outer-wall speeds).
- Path planning within polygons is a well-studied problem but non-trivial to implement robustly, especially with concave polygons and holes.

**Risks:**
- **High risk if implemented incorrectly.** An erroneous combing path could route the nozzle through infill gaps or off the print surface entirely, causing collisions with printed parts.
- Mitigation: combing should only activate when the polygon model confidence is high (closed, well-formed perimeters). Fall back to straight-line travel with retraction when uncertain.

### 2.5 Recommended Implementation Order for Travel Optimization

1. **v2.0:** Nearest-neighbor island reordering (Section 2.2). Foundational, moderate gains, manageable risk.
2. **v2.x:** 2-opt refinement (Section 2.3). Incremental improvement on top of nearest-neighbor.
3. **v3+:** Combing (Section 2.4). High complexity, best deferred until polygon model infrastructure exists for other features (support analysis, overhang detection).

---

## 3. Arc Fitting (G2/G3 Replacement)

### 3.1 Problem Statement

Slicers approximate curves as sequences of short linear G1 segments. A circle with 0.1mm resolution at 50mm diameter produces ~1,570 G1 commands. The same circle can be represented as a single G2 or G3 arc command. This matters for:

- **File size:** Arc commands compress curves by 10-100x in terms of line count.
- **Communication bandwidth:** Serial connections (115200 baud) can starve the motion planner on fast curves, causing micro-stuttering. Fewer commands means less bandwidth pressure.
- **Motion quality:** Firmware that supports arc interpolation (Marlin with `ARC_SUPPORT`, Klipper) can produce smoother motion than chained linear segments, because the firmware interpolates the arc at its native resolution rather than following the slicer's segmentation.

### 3.2 Detection Algorithm

**Input:** A sequence of G1 moves on the same Z-plane (no Z change), with or without extrusion.

**Goal:** Identify subsequences of 3 or more consecutive G1 moves that lie on a common circular arc within a configurable tolerance.

**Algorithm (sliding window arc detection):**

1. Maintain a sliding window of consecutive G1 moves on the XY plane.
2. For each window of 3+ points, attempt to fit a circle:
   - Given 3 non-collinear points, compute the unique circumscribed circle (center and radius) using the perpendicular bisector method.
   - For windows of 4+ points, use least-squares circle fitting and check that the maximum deviation of any point from the fitted circle is below the tolerance threshold.
3. Extend the window as long as the tolerance holds. When the next point exceeds tolerance, emit the accumulated arc.
4. Determine arc direction (CW = G2, CCW = G3) from the cross product of consecutive segment vectors.
5. Emit the arc command: `G2/G3 X<end> Y<end> I<center_offset_x> J<center_offset_y> E<total_extrusion> F<feedrate>`.

**Circle fitting from 3 points (exact formula):**

Given points A, B, C:
```
D = 2 * (Ax * (By - Cy) + Bx * (Cy - Ay) + Cx * (Ay - By))
Ux = ((Ax^2 + Ay^2) * (By - Cy) + (Bx^2 + By^2) * (Cy - Ay) + (Cx^2 + Cy^2) * (Ay - By)) / D
Uy = ((Ax^2 + Ay^2) * (Cx - Bx) + (Bx^2 + By^2) * (Ax - Cx) + (Cx^2 + Cy^2) * (Bx - Ax)) / D
R = sqrt((Ax - Ux)^2 + (Ay - Uy)^2)
```

When D is near zero, the points are collinear -- not an arc (see Section 6 for collinear merge instead).

### 3.3 Tolerance Thresholds

The tolerance parameter controls the maximum allowed deviation of any original point from the fitted arc. This is the most critical tuning parameter.

| Tolerance | Use Case | Risk |
|---|---|---|
| 0.005 mm | Ultra-conservative. Nearly lossless. Minimal arc detection on already-smooth curves. | Negligible |
| 0.01 mm | Conservative default. Below the resolution of most FDM printers (layer height 0.1-0.3mm, nozzle 0.4mm). | Very low |
| 0.02 mm | Moderate. Good balance of detection rate and fidelity. | Low |
| 0.05 mm | Aggressive. Visible deviation possible on fine details. | Medium |
| 0.1 mm | Very aggressive. Only appropriate for draft prints. | High |

**Recommended default: 0.01 mm.** Configurable via `--arc-tolerance <mm>`.

### 3.4 Edge Cases and Constraints

- **Extrusion consistency:** All G1 moves in an arc candidate must have consistent extrusion rate (E per mm of travel). If extrusion varies (e.g., pressure advance compensation in slicer output), the arc should not be merged because the firmware will apply uniform extrusion across the arc.
- **Feed rate consistency:** All moves in the arc must share the same feed rate, or feed rate must be absent (inherited from prior command). Mixed feed rates break the arc.
- **Z changes:** Arc fitting applies only to moves on the same Z plane. Helical arcs (G2/G3 with Z component) are supported by some firmware but are uncommon in slicer output and should be a separate, opt-in feature.
- **Minimum arc segment count:** Require at least 3 original G1 moves to form an arc. Two points always lie on infinitely many circles.
- **Minimum arc angle:** Arcs spanning less than ~15 degrees of arc are not worth replacing -- the file size savings are minimal and the risk of deviation is proportionally higher.
- **Maximum radius:** Reject arcs with radius > 1000 mm. These are effectively straight lines and the I/J offsets would have poor numerical precision.
- **Full circles:** A complete 360-degree arc is valid G2/G3 but some firmware handles it poorly. Consider splitting into two semicircles.

### 3.5 Firmware Compatibility

| Firmware | G2/G3 Support | Notes |
|---|---|---|
| **Marlin** | Yes, with `ARC_SUPPORT` enabled in Configuration_adv.h | Default in most Marlin builds. `MM_PER_ARC_SEGMENT` controls interpolation resolution (default 1mm). Supports I/J/R formats. |
| **Klipper** | Yes, native `gcode_arcs` module | Enabled by default. Internally linearizes arcs back to segments using `resolution` config parameter (default 1mm). |
| **RepRapFirmware (Duet)** | Yes, native support | Full G2/G3 with I/J/R. Handles helical arcs (with Z). |
| **Smoothieware** | Yes | Native arc support with configurable resolution. |
| **Prusa Buddy (MK4, XL)** | Yes | Marlin-based, `ARC_SUPPORT` enabled by default. |
| **Bambu Lab** | Limited | Custom firmware. G2/G3 reported to work but not officially documented. **Do not enable by default for Bambu printers.** |
| **Sailfish** | No | Legacy firmware for MakerBot clones. G2/G3 will be interpreted as unknown commands and likely ignored or cause errors. |

**Safety strategy:** Arc fitting must be disabled by default and enabled with `--arc-fit` or `--optimize arcs`. When enabled, emit a warning if the detected dialect (from slicer comments in the G-Code header) is known to have limited or no arc support. The `--dialect` flag allows the user to override auto-detection.

### 3.6 Expected Gains

- **File size reduction:** 15-40% on curved models (vases, organic shapes, cylinders). Negligible on rectilinear prints.
- **Print quality:** Measurably smoother curves on firmware that performs true arc interpolation. No improvement on firmware that re-linearizes (Klipper default behavior, though Klipper's resolution may differ from slicer's).
- **Print speed:** Marginal improvement from reduced command parsing overhead. More significant on serial-connected printers (not USB or networked).
- **Print time:** Typically < 1% reduction. The dominant factor is extrusion speed, not command count.

### 3.7 Implementation Approach for GCode-Sentinel

1. **Extend the AST** with `ArcMoveCW` and `ArcMoveCCW` variants (see [Appendix A](#appendix-a-ast-extension-requirements)).
2. **Extend the emitter** to serialize G2/G3 commands with I/J offset format.
3. **Implement the arc detector** as an optimizer pass: input `Vec<Spanned<GCodeCommand>>`, output the same with qualifying G1 sequences replaced by arc commands.
4. **Add the `--arc-fit` and `--arc-tolerance` CLI flags.**
5. **Golden-file tests:** Known curves (quarter circle, semicircle, full circle, S-curve, spiral) with expected G2/G3 output.

---

## 4. Retraction Optimization

### 4.1 Problem Statement

Retraction (pulling filament back into the nozzle before a travel move, then pushing it forward at the destination) prevents stringing and oozing. However, every retraction cycle:

- Adds ~0.2-0.5 seconds of dwell time (retraction speed + prime speed + any Z-hop).
- Grinds the filament slightly, reducing grip over many cycles.
- Can introduce under-extrusion at the start of the next extrusion segment if prime amount is not perfectly calibrated.

Slicers are generally conservative: they retract before every travel move regardless of context. Many of these retractions are unnecessary.

### 4.2 Technique: Detecting Unnecessary Retractions

**Unnecessary retraction scenarios:**

1. **Short travel over solid infill:** If the nozzle travels less than N mm (typically 2-5mm) and the travel path is over already-printed solid infill, ooze will be deposited on infill (invisible) rather than the surface. Retraction is unnecessary.

2. **Travel within the same island:** When the nozzle moves from one part of an island to another without crossing an exterior boundary (e.g., moving from the end of an infill line to the start of the next), retraction is unnecessary because any ooze stays within the already-printed region.

3. **Consecutive retractions without extrusion:** Some slicers emit retract-travel-retract sequences where two travel moves happen in succession (e.g., a wipe move followed by a travel move). The second retraction is redundant because the filament is already retracted.

**Detection algorithm:**

For each retraction event (negative E move or M-code retraction):
1. Find the corresponding travel move(s) following the retraction.
2. Measure the travel distance.
3. Determine whether the travel crosses a perimeter boundary (requires the same polygon model as combing -- or a simpler heuristic: check if travel distance is below a threshold and both endpoints are over infill Z-height).
4. If the retraction is deemed unnecessary, remove the retraction/prime pair and convert the retraction-related Z-hop (if any) to a flat travel.

**Expected gains:**
- 2-8% print time reduction on prints with many small islands or complex infill patterns.
- Reduced filament grinding, especially on long prints with flexible filaments.
- Reduced under-extrusion artifacts at segment starts.

**Implementation difficulty:** Medium-High.
- Parsing retraction events requires understanding both firmware-level retraction (G1 E-<value> or G10/G11) and slicer-configured retraction embedded in G1 moves.
- The "over solid infill" check requires layer geometry awareness that is not yet implemented.
- Simpler version (distance-only threshold) is achievable at Medium difficulty.

**Risks:**
- **Medium-High.** Removing a necessary retraction causes stringing or blobbing on the print surface. The consequences are cosmetic (not mechanical failure) but can ruin a print's visual quality.
- Mitigation: conservative default threshold (only remove retractions on travels < 1.5 mm), user-configurable, and always preserving retractions before perimeter segments.

### 4.3 Technique: Retraction Distance Tuning by Filament Type

**Description:** Analyze the retraction distances in the G-Code and compare them against known-good values for the filament type (detected from slicer comments or specified by the user).

| Filament | Bowden Retraction | Direct Drive Retraction |
|---|---|---|
| PLA | 4-7 mm | 0.5-1.5 mm |
| PETG | 4-6 mm | 1.0-2.5 mm |
| TPU/Flex | 3-5 mm | 0.5-2.0 mm (slow) |
| ABS | 5-7 mm | 0.5-1.5 mm |
| Nylon | 5-8 mm | 1.5-3.0 mm |

**This is advisory, not automatic.** GCode-Sentinel would emit `Info`-level diagnostics when retraction distances fall outside the expected range for the detected filament type. It would NOT automatically modify retraction distances, because the correct value depends on hotend geometry, bowden tube length, and filament brand -- information not available in the G-Code.

**Implementation difficulty:** Low.
- Parsing retraction amounts from G1 E moves is straightforward.
- Filament type detection from slicer comments (e.g., `; filament_type = PLA`) is heuristic but reliable for major slicers.

**Risks:** Low. Advisory only; no G-Code modification.

### 4.4 Technique: Z-Hop Optimization

**Description:** Z-hop (raising the nozzle during travel) prevents the nozzle from dragging across the print surface. However, Z-hop on every travel move is excessive. Z-hop is most beneficial:

- When travelling over the top surface (visible exterior).
- When travelling over tall thin features that could be knocked off by the nozzle.
- On the first layer (to avoid disturbing bed adhesion on nearby parts).

Z-hop is unnecessary:
- When travelling within the same island over infill.
- When the travel distance is very short (< 1 mm).
- When the previous and next extrusion segments are at the same Z height (no elevation risk).

**Detection algorithm:**
1. For each Z-hop event (G1 Z+<offset> before travel, G1 Z-<offset> after travel):
2. Evaluate whether the travel path requires Z-hop based on context:
   - Travel distance < configurable threshold (default 1.5 mm) and no perimeter crossing: remove Z-hop.
   - Travel over infill only: remove Z-hop.
   - Travel over top surface or thin features: keep Z-hop.
3. Remove the Z-raise and Z-lower commands for unnecessary Z-hops.

**Expected gains:**
- 1-3% print time reduction (each Z-hop adds ~0.1-0.3 seconds).
- Slightly cleaner top surfaces (fewer Z-hop witness marks at travel start/end points).

**Implementation difficulty:** Medium.
- Z-hop detection is straightforward (paired Z moves surrounding a non-extruding travel).
- Context evaluation (over infill vs. top surface) requires layer geometry analysis.
- Simplified version (distance-only threshold) is Low difficulty.

**Risks:**
- **Medium.** Removing Z-hop when the nozzle would drag across a printed surface causes surface marks or, on tall thin features, can knock the part off the bed. Mitigation: conservative threshold, only remove Z-hop for very short travels within the same island.

### 4.5 Technique: Wipe-Before-Retract Detection

**Description:** Some slicers perform a "wipe" move -- a short extrusion-less move along the just-printed perimeter before retracting. This deposits ooze on the perimeter interior rather than leaving a blob at the retraction point. GCode-Sentinel can detect whether wipe is configured and warn if it is missing on perimeter-to-travel transitions.

**This is advisory only.** Wipe parameters are slicer configuration; modifying them in post-processing is fragile.

**Implementation difficulty:** Low (detection and advisory).

**Risks:** Negligible (advisory only).

---

## 5. Speed/Feed Rate Optimization

### 5.1 Problem Statement

Slicers emit feed rate (F parameter) values that represent *requested* speeds, not *achieved* speeds. The firmware's motion planner applies acceleration and jerk limits, meaning the nozzle rarely reaches the commanded speed on short segments. This creates two problems:

1. **Unreachable speeds:** A G1 move commanding F6000 (100 mm/s) over a 2mm segment will never reach 100 mm/s if the printer's acceleration is 500 mm/s^2. The firmware accelerates, immediately decelerates, and peaks at perhaps 30 mm/s.
2. **Sudden speed changes:** A sequence like F3000 -> F6000 -> F3000 causes the motion planner to repeatedly accelerate and brake, producing vibration and ringing artifacts.

### 5.2 Technique: Acceleration-Aware Feed Rate Smoothing

**Description:** Analyze the feed rate profile across consecutive moves and smooth sudden transitions. Specifically:

1. Calculate the *achievable* peak speed for each move segment given the segment length and the machine's acceleration limit.
2. If the commanded speed exceeds the achievable speed, emit an `Info` diagnostic noting the segment is acceleration-limited.
3. Optionally, reduce the commanded feed rate to the achievable value. This does not change print time (the firmware would limit it anyway) but produces cleaner G-Code and more accurate print time estimates.

**Achievable speed formula:**
```
v_max = sqrt(2 * acceleration * distance)
```
If `v_max < F_commanded`, the segment is acceleration-limited.

For a trapezoidal velocity profile (accelerate, cruise, decelerate):
```
t_accel = v_cruise / acceleration
d_accel = 0.5 * acceleration * t_accel^2
d_cruise = distance - 2 * d_accel
```
If `d_cruise < 0`, the segment is a pure triangle profile (never reaches cruise speed).

**Expected gains:**
- More accurate print time estimates (5-15% improvement in estimate accuracy).
- Slightly smoother motion on printers with aggressive jerk settings.
- No change in actual print time for well-configured firmware.

**Implementation difficulty:** Medium.
- Requires knowledge of the printer's acceleration and jerk limits, which are not always present in the G-Code. Must be provided via `--acceleration <mm/s^2>` or machine profile config.
- The kinematics model is straightforward but must handle corner cases (very short segments, direction changes, multi-axis moves).

**Risks:**
- **Low.** Reducing commanded speed to achievable speed is always safe -- the firmware would have done the same. However, if the acceleration parameter is set incorrectly (too high), the tool could reduce speeds below what the printer can actually achieve, slowing the print.
- Mitigation: this optimization should be advisory-only by default, with an opt-in flag for actual G-Code modification.

### 5.3 Technique: Volumetric Flow Rate Normalization

**Description:** Different regions of a print have different volumetric flow demands. A solid infill line at 60 mm/s through a 0.4mm nozzle at 0.2mm layer height demands:

```
flow = speed * nozzle_width * layer_height = 60 * 0.4 * 0.2 = 4.8 mm^3/s
```

If the hotend can sustain a maximum of 12 mm^3/s, the print is fine. But consider a 0.6mm nozzle at 0.3mm layer height at 80 mm/s:

```
flow = 80 * 0.6 * 0.3 = 14.4 mm^3/s -- exceeds hotend capacity
```

The result is under-extrusion, weak layer adhesion, and potential extruder skipping.

**Detection approach:**
1. Track the current nozzle width and layer height (from slicer comments or user input).
2. For each extrusion move, compute the volumetric flow rate.
3. Compare against a configurable maximum (default: 12 mm^3/s for standard hotends, 24 mm^3/s for high-flow hotends).
4. Emit `Warning` diagnostics for segments exceeding the limit.
5. Optionally, reduce the feed rate on over-flow segments to bring flow within limits: `F_corrected = max_flow / (nozzle_width * layer_height)`.

**Expected gains:**
- Prevents under-extrusion on aggressive speed profiles.
- Improves layer adhesion reliability.
- May increase print time on segments that were previously under-extruding (but those segments were producing bad output anyway).

**Implementation difficulty:** Medium.
- Volumetric flow calculation is straightforward.
- Requires nozzle width and layer height as inputs (not always available in G-Code).
- Speed correction is a simple proportional adjustment.

**Risks:**
- **Low for advisory mode.** Warning about flow rate violations is always safe.
- **Medium for auto-correction.** Reducing feed rate without understanding the slicer's intent (e.g., the slicer may have already compensated with reduced extrusion width) could cause over-extrusion.
- Mitigation: advisory by default, auto-correction behind `--normalize-flow` flag.

### 5.4 Technique: Pressure Advance Tuning Suggestions

**Description:** Pressure advance (Klipper) / Linear Advance (Marlin) compensates for the pressure buildup and release in the hotend during acceleration and deceleration. When configured correctly, it eliminates bulging at corners and under-extrusion at the start of lines.

GCode-Sentinel can analyze G-Code for patterns that indicate pressure advance misconfiguration:

1. **Corner bulging pattern:** Extrusion moves followed by sharp direction changes (> 90 degrees) without corresponding E-axis adjustment. Suggests pressure advance is too low.
2. **Start-of-line under-extrusion:** Extrusion starts at full speed without a ramp-up. Suggests pressure advance is too high or not configured.
3. **Inconsistent E-axis behavior at speed changes:** If the slicer is already compensating for pressure (some do), additional firmware-level pressure advance would double-compensate.

**This is purely advisory.** Pressure advance tuning requires iterative testing on the physical printer. GCode-Sentinel can flag *potential* issues and suggest investigation.

**Implementation difficulty:** Medium-High.
- Requires understanding of the E-axis behavior in relation to speed changes.
- Pattern detection for "corner bulging" requires analyzing move direction changes and correlating with extrusion amounts.
- The advisory output must be carefully worded to avoid false confidence.

**Risks:** Low (advisory only, no G-Code modification).

---

## 6. Redundant Move Elimination (Beyond v1)

### 6.1 Recap: v1 Zero-Delta Moves

v1 already detects and removes moves where all specified coordinates match the current print head state (e.g., `G1 X10 Y20` when the head is already at X10 Y20). This section covers additional elimination techniques.

### 6.2 Technique: Collinear Move Merging

**Description:** Three or more consecutive G1 moves that lie on the same line can be merged into a single move from the first point to the last point. For example:

```
G1 X10 Y10 E1.0 F1200    ; Point A
G1 X20 Y20 E2.0 F1200    ; Point B (collinear with A->C)
G1 X30 Y30 E3.0 F1200    ; Point C
```

Can be reduced to:
```
G1 X30 Y30 E3.0 F1200    ; A -> C directly
```

**Detection algorithm:**
1. For each consecutive triple of G1 moves (A, B, C), compute the collinearity test:
   ```
   cross = (Bx - Ax) * (Cy - Ay) - (By - Ay) * (Cx - Ax)
   ```
   If `|cross| < tolerance * max(|AC|, 1.0)`, the points are collinear.
2. Verify that all three moves have the same feed rate (or the middle point's feed rate can be safely dropped).
3. Verify that extrusion is linearly proportional to distance (constant flow rate across all three segments). Compute expected E at point B based on linear interpolation between A and C; if the actual E deviates by more than the tolerance, the points are not mergeable.
4. Extend: if A-B-C are collinear, check if C-D is also collinear with A-C. Continue extending the run.

**Expected gains:**
- File size reduction: 5-15% on prints with many short collinear segments (typical in slicer output for straight walls).
- Print quality: negligible change (the merged move is geometrically identical).
- Print time: negligible (firmware motion planner already merges these internally on most controllers).

**Implementation difficulty:** Low-Medium.
- The collinearity test is simple geometry.
- The extrusion proportionality check requires care to avoid breaking flow rate.
- Must handle moves with partial axis specification (e.g., only X changes on some, only Y on others).

**Risks:**
- **Low.** The merged move is geometrically equivalent to the original sequence. The only risk is incorrect collinearity detection (merging moves that are not truly collinear), mitigated by the tolerance threshold.
- Extrusion consistency check prevents merging moves with variable flow rate (e.g., slicer-generated pressure advance compensation).

### 6.3 Technique: Consecutive Same-Axis Move Merging

**Description:** When two consecutive G1 moves affect only the same single axis, the first move is redundant:

```
G1 X10 F1200
G1 X20 F1200
```

The nozzle would travel to X10 and then immediately to X20. The intermediate stop at X10 is unnecessary unless the firmware uses it as a deceleration point (which it does in practice, based on look-ahead buffer depth). This optimization is only safe when:

- Both moves have the same feed rate.
- No extrusion occurs on either move (both are travel moves).
- No other axis changes between them.
- The moves are truly consecutive (no intervening commands of any kind).

With extrusion, the intermediate point affects the extrusion profile and must be preserved.

**Expected gains:**
- Marginal file size reduction.
- Marginal reduction in motion planner deceleration points (printer-dependent).

**Implementation difficulty:** Low.
- Simple pattern matching on consecutive commands.

**Risks:**
- **Low for travel-only moves.** The motion planner would decelerate at X10 and re-accelerate to X20 in the original; the merged move produces continuous motion to X20.
- **High for extrusion moves.** Removing intermediate extrusion points changes the deposition pattern. Only apply to non-extruding moves.

### 6.4 Technique: Redundant Feed Rate Elimination

**Description:** When the feed rate (F parameter) is specified on every move but does not change between consecutive moves, the repeated F parameter is redundant. G-Code feed rate is modal -- once set, it persists until changed.

```
G1 X10 Y10 E1.0 F1200
G1 X20 Y20 E2.0 F1200    ; F1200 is redundant
G1 X30 Y30 E3.0 F1200    ; F1200 is redundant
```

Becomes:
```
G1 X10 Y10 E1.0 F1200
G1 X20 Y20 E2.0
G1 X30 Y30 E3.0
```

**Expected gains:**
- File size reduction: 5-10% (F parameter appears on most G1 lines in typical slicer output).
- Print quality/time: zero change. Firmware behavior is identical.

**Implementation difficulty:** Low.
- Track current modal feed rate state. If a move's F matches the current state, omit F from the emitted command.

**Risks:**
- **Very low.** Modal feed rate is part of the G-Code specification. All firmware implements it identically.
- One edge case: if the output file is later manually edited and a line is inserted before the second move, the feed rate would be inherited from the inserted line, not the intended F1200. This is a workflow concern, not a correctness concern for GCode-Sentinel.

### 6.5 Technique: Redundant G90/G91 Elimination

**Description:** Remove consecutive identical mode-setting commands. If `G90` (absolute mode) is already active, a subsequent `G90` is redundant.

**Implementation difficulty:** Low. Track current mode state; omit redundant mode commands.

**Risks:** Very low. Same modal behavior as feed rate.

---

## 7. Temperature Tower / Calibration Detection

### 7.1 Problem Statement

Temperature towers and calibration prints are special G-Code files designed to test printer settings at varying parameters. A temperature tower, for example, prints multiple sections at different nozzle temperatures. Users sometimes accidentally send these to a printer as a real print, or want automated analysis of the results.

### 7.2 Technique: Temperature Tower Detection

**Detection heuristics:**

1. **Staircase temperature pattern:** The G-Code contains multiple `M104 S<temp>` or `M109 S<temp>` commands at regular Z-height intervals, with temperatures varying in a monotonic or patterned sequence (e.g., 190, 195, 200, 205, 210, ..., or descending).
2. **Known slicer markers:** PrusaSlicer and OrcaSlicer embed comments like `; temperature tower` or `; calibration` in their output.
3. **Geometric pattern:** A temperature tower typically has a small XY footprint repeated at constant intervals along Z, with no infill variation.

**Output:** An `Info`-level diagnostic annotating the file as a probable temperature tower, listing the temperature steps and corresponding Z-height ranges.

```
[info] line 1: I001 -- Probable temperature tower detected
  Step 1: Z 0.0-10.0mm @ 220C
  Step 2: Z 10.0-20.0mm @ 215C
  Step 3: Z 20.0-30.0mm @ 210C
  ...
```

### 7.3 Technique: Calibration Improvement Suggestions

Beyond detection, GCode-Sentinel can offer suggestions:

- **Missing temperature stabilization:** If `M104` (non-waiting) is used instead of `M109` (wait for temperature) before a tower section change, warn that the temperature may not stabilize in time.
- **Section height:** If sections are very short (< 5mm), warn that the temperature may not stabilize within the section, producing misleading results.
- **Cooling fan consistency:** If the fan speed varies during the tower (not intended as a fan tower), flag it.

**Implementation difficulty:** Low-Medium.
- Temperature pattern detection is straightforward (scan M104/M109 commands and correlate with Z changes).
- Slicer comment detection is simple string matching.
- Suggestion generation requires domain knowledge encoded as rules.

**Risks:** Negligible (advisory only).

### 7.4 Other Calibration Prints

The same pattern-detection approach generalizes to:
- **Retraction towers:** Varying retraction distance at regular Z intervals.
- **Speed towers:** Varying feed rate at regular Z intervals.
- **Flow towers:** Varying extrusion multiplier at regular intervals.
- **PA/LA calibration patterns:** Specific geometric patterns (Klipper PA calibration, Marlin LA calibration) with identifiable line-to-speed ratios.

Detection of these is lower priority but follows the same architectural pattern.

---

## 8. Layer Time Normalization

### 8.1 Problem Statement

Small features (thin pillars, small circles) have very short per-layer print times. When a layer prints in under 5-10 seconds, the previous layer has not cooled sufficiently, causing:

- Thermal deformation (the nozzle pushes around still-soft material).
- Poor surface quality (glossy or melted appearance).
- Print failure on tall thin features (accumulated heat causes catastrophic deformation).

Slicers typically address this with "minimum layer time" settings, but users sometimes disable this, or the slicer's implementation (slowing the entire layer uniformly) is suboptimal.

### 8.2 Technique: Minimum Layer Time Enforcement

**Algorithm:**

1. During simulation, compute the print time for each layer (sum of move distances divided by feed rates, ignoring acceleration for a lower bound, or using the acceleration model from Section 5.2 for a more accurate estimate).
2. For layers below the minimum threshold (configurable, default 10 seconds):
   - Calculate the speed reduction factor: `factor = actual_time / minimum_time`.
   - Apply the factor to all *extrusion* move feed rates on that layer (not travel moves -- those should remain fast to reduce ooze).
   - Alternatively, insert a `G4 P<ms>` dwell command at the end of the layer to pad the time. This is simpler but less effective because it parks the nozzle over the print, potentially causing heat damage at the dwell point.
3. Emit a diagnostic noting which layers were adjusted and by how much.

**Speed reduction vs. dwell trade-off:**

| Approach | Pro | Con |
|---|---|---|
| Reduce extrusion speed | Distributes cooling time evenly across the layer; nozzle keeps moving | May affect layer adhesion if speed drops too low (< 10 mm/s) |
| Insert G4 dwell at layer end | Simple to implement; no feed rate changes | Heat concentrates at dwell point; can cause blob or burn mark |
| Lift and wait (G4 + Z-hop) | Moves nozzle away from print during wait | Z-hop + return adds retraction cycle; still localizes cooling |

**Recommended approach:** Speed reduction for layers with time > 3 seconds (enough to be meaningful when slowed). Dwell with Z-hop for extremely short layers (< 3 seconds) where speed reduction alone would require impractically slow speeds.

**Expected gains:**
- Significant quality improvement on small/thin features.
- Prevents print failures on tall pillars and small circles.

**Implementation difficulty:** Medium.
- Layer time calculation reuses existing simulation infrastructure.
- Speed reduction requires modifying F parameters on extrusion moves within affected layers.
- The layer boundary detection must be robust (Z changes, slicer layer comments).

**Risks:**
- **Medium.** Excessive speed reduction can cause over-extrusion (more material deposited per mm of travel if the extruder does not compensate). Most slicers configure volumetric flow, not linear speed, so the extrusion amount per mm of filament distance remains correct. But firmware-level pressure advance may behave differently at very low speeds.
- Mitigation: enforce a minimum speed floor (default 10 mm/s). Log all changes for user review.

### 8.3 Technique: M73 Progress Marker Insertion

**Description:** `M73 P<percent> R<minutes_remaining>` tells the printer's display how much of the print is complete and how long remains. Many slicers include these, but some do not, or they are inaccurate.

**Algorithm:**
1. Compute cumulative print time through the simulation pass.
2. Insert `M73` commands at regular intervals (every 1% of progress, or every N layers).
3. If existing M73 commands are present, either preserve them or replace them with recalculated values (user-configurable).

**Expected gains:**
- Accurate progress display on the printer's screen.
- Better time-remaining estimates than the slicer provides (especially if optimizations have changed the print time).

**Implementation difficulty:** Low.
- The simulation already computes cumulative time.
- Inserting commands at computed positions is straightforward.

**Risks:**
- **Very low.** M73 is a display-only command with no effect on motion or extrusion. The worst case is an inaccurate progress display.

---

## 9. Support Structure Analysis

### 9.1 Problem Statement

Support structures hold up overhanging geometry that would otherwise droop or fail during printing. Slicers generate support based on overhang angle thresholds, but the generated support may have problems:

- **Reachability:** Support pillars themselves may have overhangs that exceed the printer's capability (support-for-support problem).
- **Insufficient support:** The slicer's overhang detection may miss areas, especially on complex organic geometry.
- **Over-support:** The slicer may generate support where it is not needed (gentle slopes that print fine without support).

### 9.2 Technique: Overhang Detection from G-Code

**Description:** Without access to the 3D model (only the G-Code), overhang analysis must be performed layer-by-layer:

1. For each extrusion segment on layer N, check whether there is supporting material on layer N-1 within a configurable distance (typically 1-2 nozzle widths).
2. If a segment on layer N has no supporting material below it, it is an unsupported overhang.

**Algorithm:**
1. Build a 2D rasterized or polygon representation of each layer's printed regions.
2. For each new layer, compare its geometry against the previous layer.
3. Regions on the new layer that extend beyond the previous layer's boundary by more than `cos(overhang_angle) * layer_height` are unsupported overhangs.
4. Emit diagnostics with the location and extent of unsupported regions.

**Expected gains:**
- Catches slicer support generation failures before printing.
- Identifies prints that need support but were sliced without it.

**Implementation difficulty:** Very High.
- Building per-layer geometry from G-Code is the hardest geometry problem in this roadmap.
- The rasterization or polygon approach requires significant infrastructure (2D boolean geometry, spatial indexing).
- False positives are likely on bridging moves (intentionally unsupported, relying on string tension).

**Risks:**
- **Low for advisory mode** (diagnostics only, no G-Code modification).
- False positive rate may be high without understanding slicer intent (bridging, support interface layers, etc.).

### 9.3 Technique: Support Reachability Analysis

**Description:** Analyze the support structure itself for structural integrity. A support pillar that leans or has overhangs beyond the printer's capability will fail, causing the supported geometry above it to also fail.

**This requires the same per-layer geometry infrastructure as overhang detection (Section 9.2).** The analysis is applied to support-labeled regions (detected from slicer comments or speed/flow heuristics) rather than the model geometry.

**Implementation difficulty:** Very High (depends on 9.2 infrastructure).

**Risks:** Low (advisory only).

### 9.4 Practical Recommendation

Support structure analysis is the most computationally and algorithmically expensive feature in this roadmap. It is recommended as a v3+ feature, after the per-layer geometry infrastructure has been developed and validated for simpler use cases (combing, retraction optimization, layer time calculation).

For v2, a much simpler heuristic is viable: **detect layers where the bounding box suddenly expands significantly** (> 5mm in any direction) compared to the previous layer. This catches obvious cases of large unsupported overhangs without requiring full geometry analysis.

---

## 10. Estimated Difficulty and Priority Matrix

### 10.1 Scoring Criteria

- **Implementation Difficulty:** Low = 1-2 weeks for one developer; Medium = 2-4 weeks; High = 1-2 months; Very High = 2+ months.
- **Print Quality Improvement:** How much the optimization improves the physical print output.
- **Time Saving:** How much print time is reduced.
- **Safety Risk:** Consequence severity if the optimization contains a bug. Low = cosmetic defect; Medium = print failure (wasted time/material); High = potential hardware damage (nozzle crash, thermal issue).

### 10.2 Matrix

| # | Technique | Difficulty | Quality Gain | Time Saving | Safety Risk | Priority |
|---|---|---|---|---|---|---|
| 6.4 | Redundant feed rate elimination | Low | None | None | Very Low | 1 (v2.0) |
| 6.5 | Redundant G90/G91 elimination | Low | None | None | Very Low | 1 (v2.0) |
| 6.2 | Collinear move merging | Low-Medium | Low | Low | Low | 2 (v2.0) |
| 8.3 | M73 progress marker insertion | Low | None | None | Very Low | 2 (v2.0) |
| 4.3 | Retraction distance advisory | Low | Low (advisory) | None | Low | 2 (v2.0) |
| 4.5 | Wipe detection advisory | Low | Low (advisory) | None | Low | 2 (v2.0) |
| 7.2 | Temperature tower detection | Low-Medium | Low (advisory) | None | Negligible | 3 (v2.x) |
| 6.3 | Same-axis move merging (travel) | Low | None | Low | Low | 3 (v2.x) |
| 2.2 | Nearest-neighbor island reorder | Medium | Medium | Medium | Medium | 4 (v2.x) |
| 3.2 | Arc fitting (G2/G3) | Medium-High | Medium | Low | Medium | 4 (v2.x) |
| 8.2 | Minimum layer time enforcement | Medium | High | Negative* | Medium | 5 (v2.x) |
| 5.2 | Feed rate smoothing (advisory) | Medium | Low (advisory) | Low | Low | 5 (v2.x) |
| 5.3 | Volumetric flow normalization | Medium | Medium | Negative* | Medium | 5 (v2.x) |
| 4.2 | Unnecessary retraction removal | Medium-High | Medium | Medium | Medium-High | 6 (v3+) |
| 4.4 | Z-hop optimization | Medium | Low-Medium | Low | Medium | 6 (v3+) |
| 2.3 | TSP 2-opt island reordering | Medium-High | Medium | Medium | Low | 7 (v3+) |
| 5.4 | Pressure advance advisory | Medium-High | Low (advisory) | None | Low | 7 (v3+) |
| 2.4 | Combing (travel within perimeter) | High | High | Low | High | 8 (v3+) |
| 9.2 | Overhang detection | Very High | High (advisory) | None | Low | 9 (v4+) |
| 9.3 | Support reachability analysis | Very High | Medium (advisory) | None | Low | 10 (v4+) |

*\* "Negative" time saving means the optimization intentionally increases print time (slower speeds for cooling / flow control) in exchange for quality.*

### 10.3 Recommended Version Groupings

**v2.0 -- Low-Risk File Optimizations (Priority 1-2)**
Focus: safe, well-understood optimizations that reduce file size and add useful metadata without modifying print behavior.
- Redundant feed rate elimination
- Redundant G90/G91 elimination
- Collinear move merging
- M73 progress markers
- Retraction distance advisory
- Wipe detection advisory

**v2.x -- Motion Optimizations (Priority 3-5)**
Focus: optimizations that modify travel paths, print speed, or insert new commands. Each requires more testing and has higher risk.
- Temperature tower detection
- Nearest-neighbor island reordering
- Arc fitting (G2/G3)
- Minimum layer time enforcement
- Feed rate smoothing (advisory first, auto-correct later)
- Volumetric flow normalization

**v3+ -- Geometry-Aware Optimizations (Priority 6-8)**
Focus: optimizations requiring per-layer geometry understanding. These share infrastructure.
- Unnecessary retraction removal (context-aware)
- Z-hop optimization (context-aware)
- TSP 2-opt improvement
- Combing
- Pressure advance advisory

**v4+ -- Advanced Analysis (Priority 9-10)**
Focus: full structural analysis of the printed geometry from G-Code.
- Overhang detection
- Support structure analysis

### 10.4 Critical Path

The primary infrastructure dependency is **per-layer geometry construction** -- building a 2D polygon/raster model of each layer from the G-Code move sequence. This infrastructure is needed by:
- Combing (Section 2.4)
- Context-aware retraction removal (Section 4.2)
- Context-aware Z-hop optimization (Section 4.4)
- Overhang detection (Section 9.2)
- Support analysis (Section 9.3)

Starting this infrastructure in v2.x (even in a simplified form) unblocks many v3+ features. Conversely, deferring it gates all geometry-aware features behind a single bottleneck.

---

## 11. G-Code Dialect Considerations

### 11.1 Dialect Overview

G-Code is not a single standard. Dialects vary across firmware and slicer ecosystems. The relevant dialects for desktop FDM printing are:

| Dialect | Firmware | Key Differences |
|---|---|---|
| **Marlin** | Marlin, Prusa Buddy, Creality | De facto standard. G2/G3 optional. M73 supported. Linear Advance via M900. |
| **Klipper** | Klipper | Extends G-Code with macros (e.g., `EXCLUDE_OBJECT_START`, `SET_PRESSURE_ADVANCE`). G2/G3 via `gcode_arcs` module. No M73 (uses `M73` via macro or `SET_DISPLAY_TEXT`). |
| **RepRapFirmware** | Duet (RRF3) | Full G2/G3 with helical support. `M568` for tool temperature (not M104). Conditional G-Code (`if`, `while` blocks). |
| **Sailfish** | MakerBot clones | Legacy. No G2/G3. Limited M-code set. Largely obsolete. |
| **Bambu Lab** | Bambu printers | Proprietary firmware. Accepts standard G-Code but many features are handled internally. G2/G3 support is unofficial. |

### 11.2 Slicer-Specific Comments

Slicers embed metadata in G-Code comments that GCode-Sentinel can use for dialect detection:

| Slicer | Header Comment Pattern | Useful Metadata |
|---|---|---|
| **PrusaSlicer** | `; generated by PrusaSlicer X.Y.Z` | Filament type, nozzle diameter, layer heights, temperatures, fan speeds |
| **OrcaSlicer** | `; generated by OrcaSlicer X.Y.Z` | Same as PrusaSlicer (fork) plus `; filament_type`, `; nozzle_diameter` |
| **Cura** | `; Generated with Cura_SteamEngine X.Y.Z` | Fewer inline comments; settings in header block |
| **Simplify3D** | `; G-Code generated by Simplify3D(R)` | Minimal inline metadata |
| **SuperSlicer** | `; generated by SuperSlicer X.Y.Z` | Same format as PrusaSlicer |
| **IdeaMaker** | `; IdeaMaker X.Y.Z` | Custom comment format |

### 11.3 Safety Classification by Optimization

**Safe across all dialects (no dialect-specific handling needed):**
- Redundant feed rate elimination (F is modal in all dialects)
- Redundant G90/G91 elimination (modal in all dialects)
- Collinear move merging (G1 is universal)
- Zero-delta move removal (G0/G1 universal)
- Same-axis move merging (G0/G1 universal)
- Layer time analysis and advisory output
- Temperature tower detection (M104/M109 universal)
- Volumetric flow advisory

**Require dialect-aware handling:**

| Optimization | Dialect Concern | Handling Strategy |
|---|---|---|
| **Arc fitting (G2/G3)** | Sailfish: no support. Bambu: unofficial. Klipper: re-linearizes internally. | Require `--arc-fit` flag. Warn on detected unsupported dialects. Add `--dialect` override. |
| **M73 progress markers** | Klipper: M73 may not be recognized unless a macro is defined. RRF: uses `M73` natively. | Detect Klipper dialect; emit `M117` (display message) as alternative. Add `--progress-format` flag. |
| **Minimum layer time (speed reduction)** | All dialects support F parameter changes. No concern. | Safe across all. |
| **Minimum layer time (G4 dwell)** | All dialects support G4. Klipper: G4 pauses all motion including fan. | Document that G4 pauses fans on Klipper, which may worsen cooling. |
| **Retraction optimization** | Marlin: G10/G11 firmware retraction. Klipper: typically G1 E-based. RRF: G10/G11. | Detect retraction method (G10/G11 vs. G1 E) from G-Code content. Handle both. |
| **Pressure advance advisory** | Marlin: M900 K<value>. Klipper: `SET_PRESSURE_ADVANCE`. RRF: M572. | Detect the relevant command in G-Code. Advisory output references the correct command for the detected dialect. |
| **Combing** | No dialect concern for the combing path itself. But retraction removal during combing must respect dialect retraction method. | Depends on retraction handling. |

### 11.4 Dialect Detection Strategy

GCode-Sentinel should implement a multi-signal dialect detection pipeline:

1. **Slicer comment scan (highest priority):** Scan the first 100 lines for slicer identification comments (see table in 11.2). Map slicer to default target firmware (PrusaSlicer -> Marlin or Klipper depending on printer profile comment).
2. **Klipper macro detection:** If any line starts with a Klipper-specific command (`EXCLUDE_OBJECT`, `SET_PRESSURE_ADVANCE`, `SET_VELOCITY_LIMIT`, `SET_RETRACTION`), classify as Klipper dialect.
3. **RRF conditional G-Code detection:** If `if`, `elif`, `while` appear as G-Code commands, classify as RepRapFirmware.
4. **G2/G3 presence detection:** If the file already contains G2/G3 commands, the target firmware supports arcs.
5. **User override:** `--dialect <marlin|klipper|rrf|sailfish|bambu>` overrides all auto-detection.

The detected dialect is stored in the analysis state and consulted by each optimization pass to determine applicability and output format.

---

## Appendix A: AST Extension Requirements

Several optimizations require extending the `GCodeCommand` enum in `src/models.rs`. This appendix collects all required extensions.

### A.1 Arc Move Commands (for Section 3)

```rust
/// `G2` -- clockwise arc move.
ArcMoveCW {
    x: Option<f64>,        // Target X
    y: Option<f64>,        // Target Y
    z: Option<f64>,        // Target Z (helical arc)
    e: Option<f64>,        // Extrusion
    f: Option<f64>,        // Feed rate
    i: Option<f64>,        // Arc center X offset from start
    j: Option<f64>,        // Arc center Y offset from start
    r: Option<f64>,        // Arc radius (alternative to I/J)
},

/// `G3` -- counter-clockwise arc move.
ArcMoveCCW {
    x: Option<f64>,
    y: Option<f64>,
    z: Option<f64>,
    e: Option<f64>,
    f: Option<f64>,
    i: Option<f64>,
    j: Option<f64>,
    r: Option<f64>,
},
```

**Note:** The I/J format (center offset) and R format (radius) are mutually exclusive. The emitter should emit I/J format by default (more widely supported and unambiguous for arcs > 180 degrees). R format should be available via `EmitConfig`.

The emitter (`src/emitter.rs`) must be extended with match arms for these variants. The existing `GCommand` variant currently captures G2/G3 as raw strings; the parser should be extended to parse them into structured fields when arc optimization is enabled.

### A.2 Dwell Command (for Section 8)

```rust
/// `G4` -- dwell (pause for a specified duration).
Dwell {
    /// Duration in milliseconds (P parameter).
    p_ms: Option<f64>,
    /// Duration in seconds (S parameter).
    s_sec: Option<f64>,
},
```

Currently captured by the generic `GCommand` variant. Promoting to a first-class variant is needed if the optimizer synthesizes dwell commands for layer time normalization.

### A.3 Progress Command (for Section 8.3)

```rust
/// `M73` -- set print progress for display.
SetProgress {
    /// Print progress percentage (0-100).
    percent: Option<f64>,
    /// Estimated remaining time in minutes.
    remaining_minutes: Option<f64>,
},
```

### A.4 Fan Speed Command (for Section 7)

```rust
/// `M106` -- set fan speed.
SetFanSpeed {
    /// Fan speed (0-255, or 0.0-1.0 on some firmware).
    speed: f64,
    /// Fan index (P parameter), default 0.
    fan_index: Option<u8>,
},

/// `M107` -- turn fan off.
FanOff {
    /// Fan index (P parameter), default 0.
    fan_index: Option<u8>,
},
```

### A.5 Firmware Retraction Commands (for Section 4)

```rust
/// `G10` -- firmware retract.
FirmwareRetract,

/// `G11` -- firmware unretract (recover/prime).
FirmwareUnretract,
```

### A.6 Implementation Strategy

Not all variants need to be added immediately. The recommended approach:

1. **v2.0:** Add `SetProgress` (for M73 insertion), promote `Dwell` from `GCommand`.
2. **v2.x:** Add `ArcMoveCW`, `ArcMoveCCW` (for arc fitting). Add `SetFanSpeed`, `FanOff` (for temperature tower analysis).
3. **v3+:** Add `FirmwareRetract`, `FirmwareUnretract` (for retraction optimization).

Each new variant requires updates to:
- `src/models.rs` -- enum definition
- `src/parser.rs` -- parsing logic (currently these would hit the `GCommand`/`MetaCommand` catch-all)
- `src/emitter.rs` -- serialization logic
- Relevant test files

---

## Appendix B: Testing Strategy for Optimizations

### B.1 Golden-File Testing

Every optimization that modifies G-Code must have golden-file tests:

1. **Input file:** A G-Code file exercising the optimization's target pattern. Stored in `tests/fixtures/optimize/<technique>/input.gcode`.
2. **Expected output:** The correctly optimized output. Stored in `tests/fixtures/optimize/<technique>/expected.gcode`.
3. **Test:** Parse input, run optimizer with specific flags, emit output, compare byte-for-byte against expected. Any difference is a test failure.

Golden files should cover:
- **Happy path:** Clear optimization opportunity, expected transformation.
- **No-op:** Input where the optimization does not apply; output should match input exactly.
- **Edge cases:** Boundary conditions documented per technique (e.g., arc fitting: exactly 3 collinear points, feed rate change mid-arc candidate, extrusion rate variation).
- **Mixed content:** Real-world G-Code with multiple optimization opportunities interspersed with non-optimizable regions.

### B.2 Property-Based Testing

For optimizations that should preserve invariants, use `proptest` to generate random G-Code sequences and verify:

- **Total extrusion preservation:** After optimization, the total E-axis movement must equal the original (within floating-point tolerance). Applies to: collinear merging, island reordering, arc fitting.
- **Bounding box preservation:** The set of XY points visited must not change (for move elimination) or the bounding box must not expand (for travel reordering).
- **No command loss:** The number of extrusion segments must not decrease (for travel-only optimizations).
- **Mode state consistency:** The positioning mode (G90/G91) and other modal states must be correct at every point in the optimized output.

### B.3 Round-Trip Testing

For AST extensions (new command variants):

1. Parse a G-Code file containing the new command type.
2. Emit the parsed AST back to G-Code text.
3. Re-parse the emitted text.
4. Assert the re-parsed AST equals the original parsed AST.

This catches parser/emitter mismatches and ensures lossless round-tripping.

### B.4 Regression Testing with Real-World Files

Maintain a corpus of real-world G-Code files from major slicers (PrusaSlicer, OrcaSlicer, Cura, Simplify3D). These are not golden files (the expected output is not pre-computed) but are used for:

- **Crash testing:** The optimizer should not panic or produce errors on any real-world input.
- **Invariant testing:** Total extrusion, bounding box, and command count invariants should hold.
- **Performance benchmarking:** Track optimization pass runtime on large files across versions.

**License note:** Real-world G-Code files generated by open-source slicers from open-source 3D models (e.g., from Thingiverse under CC licenses) are suitable for the test corpus. Do not include proprietary models.

### B.5 Post-Optimization Validation

As stated in the safety philosophy (Section 1), the analyzer must run on the optimized output:

1. Run the full analysis pipeline on the optimizer's output.
2. Compare the analysis results against the pre-optimization analysis.
3. Flag any new diagnostics (especially `Error` severity) that were not present before optimization.
4. If new errors appear, the optimization is rejected and the original file is preserved.

This is the safety net that catches optimizer bugs before they reach hardware.

---

## Appendix C: References and Prior Art

### C.1 Existing Tools

| Tool | Language | Relevant Features | License |
|---|---|---|---|
| **ArcWelder** (FormerLurker) | C++ | Arc fitting with configurable tolerance. Industry standard for G2/G3 conversion. | AGPL-3.0 (not compatible -- do not copy code) |
| **ArcStraightener** | Python | Reverse of ArcWelder: converts arcs back to line segments. Useful for testing. | MIT |
| **Slic3r / PrusaSlicer** | C++/Perl | Island ordering, retraction optimization, minimum layer time. Reference implementation for slicer-side optimizations. | AGPL-3.0 (not compatible -- study algorithms only) |
| **Cura** (Ultimaker) | Python/C++ | Combing, retraction, support generation. | LGPL-3.0 (study algorithms only; LGPL code cannot be statically linked into MIT project) |
| **CNC.js** | JavaScript | G-Code parsing and visualization. | MIT |
| **gcode-rs** | Rust | G-Code parsing library. | MIT (potential dependency or reference) |

**Important:** ArcWelder and Slic3r/PrusaSlicer are AGPL-licensed. Their source code may be studied for algorithmic understanding, but no code may be copied or adapted into GCode-Sentinel (MIT). All implementations must be written independently based on published algorithms and the G-Code specification.

### C.2 Algorithms and Literature

- **TSP Heuristics:** Lin-Kernighan heuristic (1973), 2-opt/3-opt local search. Well-documented in combinatorial optimization textbooks.
- **Circle Fitting:** Kasa method (algebraic fit), Taubin method (geometric fit). For 3-point exact fit, the circumscribed circle formula is standard analytic geometry.
- **Visibility Graph Path Planning:** De Berg et al., "Computational Geometry: Algorithms and Applications" (Springer). Relevant for combing implementation.
- **Trapezoidal Motion Profile:** Standard in CNC and robotics control. Textbook kinematics: acceleration phase, cruise phase, deceleration phase.
- **G-Code Specification:** NIST RS274NGC Interpreter (the original), extended by LinuxCNC, Marlin, and Klipper documentation.

### C.3 Firmware Documentation

- **Marlin G-Code reference:** https://marlinfw.org/meta/gcode/
- **Klipper G-Code reference:** https://www.klipper3d.org/G-Codes.html
- **RepRapFirmware G-Code reference:** https://docs.duet3d.com/en/User_manual/Reference/Gcodes
- **RepRap G-Code wiki:** https://reprap.org/wiki/G-code (community-maintained, covers multiple firmware)

---

*This document is a living reference. As techniques are implemented, their sections should be updated with implementation notes, measured performance data, and lessons learned.*
