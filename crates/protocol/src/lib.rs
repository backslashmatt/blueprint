//! The serialisable request/response API between the engine and any front
//! end (REQUIREMENTS §5.3). The UI reaches the engine only through this crate.

/// Version of the protocol. Bumped on any incompatible change.
pub const PROTOCOL_VERSION: u32 = 0;
