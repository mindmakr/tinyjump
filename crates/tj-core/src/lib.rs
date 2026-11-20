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
pub use {compile::lawful_bases, compile::reveals, flow::Reveal};
pub use runtime::protocol::{Input, Output, PatchOp, Target};
pub use runtime::store::{MemStore, Store};
pub use runtime::{BootError, Options, Runtime};
