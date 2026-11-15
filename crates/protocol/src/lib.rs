//! The serialisable request/response API between the engine and any front
//! end (REQUIREMENTS §5.3). The UI reaches the engine only through this crate.

/// Version of the protocol. Bumped on any incompatible change; 1 added the
/// model types (`docs/model-json.md`).
pub const PROTOCOL_VERSION: u32 = 1;

/// The architecture model types, re-exported so front ends depend on
/// `protocol` only (REQUIREMENTS §5.3).
pub use blueprint_model as model;
