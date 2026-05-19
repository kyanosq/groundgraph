//! SpecSlice engine.
//!
//! MVP-0 surface area:
//! - [`EngineConfig`] — workspace-level config persisted to `.specslice.yaml`.
//! - [`init_repository`] — generate the config file and graph database.

pub mod config;
pub mod export;
pub mod init;

pub use config::EngineConfig;
pub use export::{export, ExportFormat, ExportOptions, ExportOutcome};
pub use init::{init_repository, InitOptions, InitOutcome};
