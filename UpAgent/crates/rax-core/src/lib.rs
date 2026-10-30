#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![forbid(unsafe_code)]

pub mod error;
pub mod lifecycle;
pub mod operation;
pub mod plan;
pub mod policy;
pub mod protocol;
pub mod result;
pub mod schema;

pub use error::{CoreInvariantError, ErrorCode, ErrorPhase, RaxError};
pub use lifecycle::{EventKind, RunState, transition};
pub use operation::*;
pub use plan::{PathKind, PathPrecondition, Plan, WorkspaceBinding};
pub use policy::{DefaultPolicy, EffectClass, EffectDecision, PolicyDecision, effects_for};
pub use protocol::{MAX_REQUEST_WIRE_BYTES, RequestEnvelope, SCHEMA_VERSION};
pub use result::*;
