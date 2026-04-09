#![no_main]
use libfuzzer_sys::fuzz_target;
use gcode_sentinel::analyzer::analyze;
use gcode_sentinel::optimizer::{optimize, OptConfig};
use gcode_sentinel::parser::parse_all;

fuzz_target!(|data: &[u8]| {
    let text = match std::str::from_utf8(data) {
        Ok(t) => t,
        Err(_) => return,
    };
    let cmds = match parse_all(text) {
        Ok(c) => c,
        Err(_) => return,
    };
    let pre_stats = analyze(cmds.iter(), None).stats;
    let config = OptConfig::default();
    let result = optimize(cmds, &config);
    let post_stats = analyze(result.commands.iter(), None).stats;
    let diff = (pre_stats.total_filament_mm - post_stats.total_filament_mm).abs();
    assert!(
        diff < 1e-6,
        "Filament not conserved: {:.6} -> {:.6} (diff {diff})",
        pre_stats.total_filament_mm, post_stats.total_filament_mm
    );
});
