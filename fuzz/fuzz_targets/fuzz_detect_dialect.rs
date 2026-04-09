#![no_main]
use libfuzzer_sys::fuzz_target;
use gcode_sentinel::dialect::{detect_dialect, Confidence, SlicerDialect};
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

    // Unknown dialect must have None confidence, and vice versa
    if result.metadata.dialect == SlicerDialect::Unknown {
        assert_eq!(
            result.metadata.confidence,
            Confidence::None,
            "Unknown dialect must have None confidence"
        );
    }
    if result.metadata.confidence == Confidence::None {
        assert_eq!(
            result.metadata.dialect,
            SlicerDialect::Unknown,
            "None confidence must have Unknown dialect"
        );
    }
});
