#![forbid(unsafe_code)]

mod composition;
mod error;
mod event;
mod id;
mod image;
mod image_policy;
mod image_prompt;
mod model;
mod profile;
mod task;

pub use composition::*;
pub use error::*;
pub use event::*;
pub use id::*;
pub use image::*;
pub use image_policy::*;
pub use image_prompt::*;
pub use model::*;
pub use profile::*;
pub use task::*;

pub const PROTOCOL_VERSION: u32 = 2;

/// Version of the task-only ingress used by the explicit `AgentSpec` path.
///
/// The older runtime envelope remains an internal execution contract for the
/// existing session engine. New callers must use `TaskEnvelopeV3`, which does
/// not carry profile, capability, effect, memory, or provider authority.
pub const TASK_ENVELOPE_V3_VERSION: u32 = 3;
