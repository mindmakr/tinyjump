//! tinyjump core: the language, the VM and the runtime.
//!
//! The core does no I/O. A host feeds it [`Input`]s with the current time and
//! carries out the [`Output`]s it returns (draw patches, run effects, persist,
//! wake up later). Every host — web, Android, iOS, Linux server — embeds this crate.

pub mod compile;
pub mod crypto;
#[cfg(feature = "check")]
pub mod flow;
pub mod host;
pub mod modules;
#[cfg(feature = "check")]
pub mod lawful_basis;
pub mod runtime;
pub mod syntax;
pub mod typeck;
pub mod ui;
pub mod vm;

pub use compile::compile;
#[cfg(feature = "check")]
pub use {
    compile::{check_remote_flows, facts, facts_with_remotes, Facts},
    flow::{Destination, Reveal, Transfer},
};

/// Loads precompiled `//# tjb:1:...` bytecode directly, or compiles `.tj` source.
pub fn load(src: &str) -> Result<vm::program::Program, Vec<syntax::Diagnostic>> {
    if let Some(decoded) = vm::program::Program::from_bundle(src) {
        decoded.map_err(|m| vec![syntax::Diagnostic::new(m, syntax::Span::default()).hint("rebuild the .tjb bundle with `tj build`")])
    } else {
        compile(src)
    }
}
pub use runtime::protocol::{Input, Output, PatchOp, Target};
pub use runtime::store::{MemStore, Store};
pub use runtime::{BootError, Options, Runtime};
