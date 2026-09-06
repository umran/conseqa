//! The orchestration harness: launching and supervising coding-agent
//! sessions against the confluence engine, and driving the design
//! workflow from natural-language prompt to verified model.
//!
//! The harness embeds the same engine the standalone daemon serves
//! (§6.1 of the confluence spec) and does not depend on any provider's
//! native subagent feature (§60): the scheduler creates logical tasks,
//! and a backend maps each task to one agent session. Notifications
//! cancel obsolete sessions early; the commit gate remains the
//! correctness mechanism.

pub mod backend;
pub mod backends;
pub mod scheduler;
pub mod supervisor;
pub mod task_prompt;
pub mod workflow;

pub use backend::*;
pub use scheduler::*;
pub use supervisor::*;
pub use workflow::*;
