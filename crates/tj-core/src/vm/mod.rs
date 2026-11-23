//! The bytecode VM: values, programs, builtins and the interpreter loop.

pub mod builtins;
pub mod bytecode;
pub mod interp;
pub mod program;
pub mod time;
pub mod value;

/// Library code compiled into every program.
#[cfg(feature = "compiler")]
pub const PRELUDE: &str = include_str!("prelude.tj");
