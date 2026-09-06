//! Concrete agent backends over provider CLIs.
//!
//! Each backend maps a logical task to a provider session and streams
//! its structured events; the portable subprocess adapter (§59) is
//! adequate for V1, so no backend depends on a provider's app-server
//! protocol. The Claude Code adapter lands first; Codex follows.

pub mod claude;
pub mod codex;
pub mod process;

pub use claude::ClaudeCliBackend;
pub use codex::CodexCliBackend;
