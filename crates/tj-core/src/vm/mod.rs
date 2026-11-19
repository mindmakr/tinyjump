//! The bytecode VM: values, programs, builtins and the interpreter loop.

pub mod builtins;
pub mod interp;
pub mod program;
pub mod value;

/// Library code compiled into every program.
pub const PRELUDE: &str = include_str!("prelude.tj");
