#![warn(clippy::pedantic)]

pub mod analyzer;
pub mod arc_fitter;
pub mod cli;
pub mod diagnostics;
pub mod dialect;
pub mod emitter;
pub(crate) mod geometry;
pub mod machine_profile;
pub mod models;
pub mod optimizer;
pub mod parser;
