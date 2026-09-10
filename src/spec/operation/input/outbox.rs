use serde::{Deserialize, Serialize};

use crate::spec::Id;

use super::MessageSelector;

/// A logical outbox consumer boundary: one message admitted to the
/// outbox may invoke this operation, and the invocation's payload is
/// that one logical message.
///
/// The L0 program stays a per-message logical machine — no batch
/// payload, batch iteration, or acknowledgement statement exists. How
/// often the runtime delivers a committed message, how consumption is
/// partitioned and ordered, and where invocations execute are
/// realization facts declared by
/// [`OutboxRuntime`](crate::spec::OutboxRuntime) against the
/// `(operation, input)` pair this input already establishes.
///
/// Several inputs may consume one outbox; each is an independent
/// logical consumer relationship with its own selection,
/// acknowledgement, and runtime facts. An outbox input has no
/// synchronous result; its normal terminal is `complete`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutboxInput {
    /// Outbox from which this input receives messages.
    pub outbox: Id,

    /// Message schemas from the outbox that may invoke this operation.
    pub messages: MessageSelector,

    /// Whether successful logical completion of an invocation
    /// triggered through this input acknowledges the triggering
    /// message for this consumer — an input-level application
    /// semantic, deliberately explicit, never an explicit program
    /// statement.
    ///
    /// Acknowledgement is consumer-relative: one input acknowledging a
    /// message says nothing about another input on the same outbox. It
    /// implies no delivery multiplicity of its own — under
    /// at-least-once delivery, failure or uncertainty before
    /// acknowledgement may admit another attempt, so it neither closes
    /// an idempotency proof nor proves duplicate collapse.
    pub acknowledge_on_success: bool,
}
