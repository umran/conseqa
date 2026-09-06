//! Per-task capability tokens.
//!
//! One high-entropy bearer capability per task (§43): the token
//! resolves to exactly one task, whose pinned snapshot, read tracker,
//! and write scope it carries. Tokens are never shared across tasks,
//! and an agent can never widen its own authority — the scheduler
//! mints tokens.

use parking_lot::RwLock;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use super::task::TaskId;

/// A 256-bit random capability, hex-encoded.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskToken(pub String);

impl TaskToken {
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];

        getrandom::fill(&mut bytes).expect("operating system randomness is available");

        let mut token = String::with_capacity(64);

        for byte in bytes {
            use std::fmt::Write;

            write!(token, "{byte:02x}").expect("writing to a String cannot fail");
        }

        Self(token)
    }
}

/// Token → task resolution.
#[derive(Default)]
pub struct TokenMap {
    tokens: RwLock<FxHashMap<String, TaskId>>,
}

impl TokenMap {
    pub fn issue(&self, task: TaskId) -> TaskToken {
        let token = TaskToken::generate();

        self.tokens.write().insert(token.0.clone(), task);

        token
    }

    pub fn resolve(&self, token: &str) -> Option<TaskId> {
        self.tokens.read().get(token).copied()
    }

    /// Re-points an existing token at a new task — how an interactive
    /// session's token rolls from one task in its chain to the next
    /// without the client's configured bearer value ever changing.
    pub fn repoint(&self, token: &str, task: TaskId) {
        self.tokens.write().insert(token.to_string(), task);
    }

    /// Drops every token resolving to `task`, ending its authority.
    pub fn revoke(&self, task: TaskId) {
        self.tokens.write().retain(|_, held| *held != task);
    }
}
