#![no_main]
use libfuzzer_sys::fuzz_target;
use gcode_sentinel::dialect::{detect_dialect, Confidence};
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
    let result = detect_dialect(&cmds, None);
    assert!(result.metadata.confidence <= Confidence::High);
});
