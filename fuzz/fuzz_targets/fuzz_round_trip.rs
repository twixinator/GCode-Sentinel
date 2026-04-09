#![no_main]
use libfuzzer_sys::fuzz_target;
use gcode_sentinel::emitter::{emit, EmitConfig};
use gcode_sentinel::models::GCodeCommand;
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
    let mut buf = Vec::new();
    if emit(&cmds, &mut buf, &EmitConfig::default()).is_err() {
        return;
    }
    let text2 = match String::from_utf8(buf) {
        Ok(t) => t,
        Err(_) => return,
    };
    let cmds2 = match parse_all(&text2) {
        Ok(c) => c,
        Err(_) => panic!("re-parse failed on emitted output"),
    };
    assert_eq!(cmds.len(), cmds2.len(), "command count changed after round-trip");
    for (a, b) in cmds.iter().zip(cmds2.iter()) {
        assert!(
            commands_semantically_equal(&a.inner, &b.inner),
            "semantic mismatch at line {}: {:?} vs {:?}", a.line, a.inner, b.inner
        );
    }
});

fn commands_semantically_equal(a: &GCodeCommand<'_>, b: &GCodeCommand<'_>) -> bool {
    use GCodeCommand::*;
    match (a, b) {
        (RapidMove { x: x1, y: y1, z: z1, f: f1 }, RapidMove { x: x2, y: y2, z: z2, f: f2 }) =>
            opts_eq(*x1, *x2) && opts_eq(*y1, *y2) && opts_eq(*z1, *z2) && opts_eq(*f1, *f2),
        (LinearMove { x: x1, y: y1, z: z1, e: e1, f: f1 }, LinearMove { x: x2, y: y2, z: z2, e: e2, f: f2 }) =>
            opts_eq(*x1, *x2) && opts_eq(*y1, *y2) && opts_eq(*z1, *z2) && opts_eq(*e1, *e2) && opts_eq(*f1, *f2),
        (SetAbsolute, SetAbsolute) | (SetRelative, SetRelative) => true,
        (SetPosition { x: x1, y: y1, z: z1, e: e1 }, SetPosition { x: x2, y: y2, z: z2, e: e2 }) =>
            opts_eq(*x1, *x2) && opts_eq(*y1, *y2) && opts_eq(*z1, *z2) && opts_eq(*e1, *e2),
        (MetaCommand { code: c1, .. }, MetaCommand { code: c2, .. }) => c1 == c2,
        (Comment { .. }, Comment { .. }) => true,
        (GCommand { code: c1, .. }, GCommand { code: c2, .. }) => c1 == c2,
        (Unknown { .. }, Unknown { .. }) => true,
        (ArcMoveCW { x: x1, y: y1, z: z1, e: e1, f: f1, i: i1, j: j1 },
         ArcMoveCW { x: x2, y: y2, z: z2, e: e2, f: f2, i: i2, j: j2 }) =>
            opts_eq(*x1, *x2) && opts_eq(*y1, *y2) && opts_eq(*z1, *z2) &&
            opts_eq(*e1, *e2) && opts_eq(*f1, *f2) && opts_eq(*i1, *i2) && opts_eq(*j1, *j2),
        (ArcMoveCCW { x: x1, y: y1, z: z1, e: e1, f: f1, i: i1, j: j1 },
         ArcMoveCCW { x: x2, y: y2, z: z2, e: e2, f: f2, i: i2, j: j2 }) =>
            opts_eq(*x1, *x2) && opts_eq(*y1, *y2) && opts_eq(*z1, *z2) &&
            opts_eq(*e1, *e2) && opts_eq(*f1, *f2) && opts_eq(*i1, *i2) && opts_eq(*j1, *j2),
        _ => false,
    }
}

fn opts_eq(a: Option<f64>, b: Option<f64>) -> bool {
    match (a, b) {
        (Some(x), Some(y)) => (x - y).abs() < 1e-4,
        (None, None) => true,
        _ => false,
    }
}
