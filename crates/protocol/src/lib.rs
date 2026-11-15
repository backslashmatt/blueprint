//! The serialisable request/response API between the engine and any front
//! end (REQUIREMENTS §5.3). The UI reaches the engine only through this crate.

/// Version of the protocol. Bumped on any incompatible change.
pub const PROTOCOL_VERSION: u32 = 0;

/// The architecture model types, re-exported so front ends depend on
/// `protocol` only (REQUIREMENTS §5.3).
pub use blueprint_model as model;
