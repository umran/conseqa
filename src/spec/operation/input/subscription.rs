use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::spec::Id;

/// A logical subscription boundary: a message admitted through it may
/// invoke this operation.
///
/// That is the whole L0 fact. How often the transport delivers such a
/// message, and where those deliveries execute, are realization facts
/// declared by
/// [`SubscriptionRuntime`](crate::spec::SubscriptionRuntime) against
/// the `(operation, input)` pair this input already establishes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscriptionInput {
    /// Topic from which this subscription receives messages.
    pub topic: Id,

    /// Message schemas from the topic that may invoke this operation.
    pub messages: MessageSelector,

    /// Whether successful logical completion of an invocation
    /// triggered through this input acknowledges the triggering
    /// message for this consumer — the same input-level application
    /// semantic an [`OutboxInput`](super::OutboxInput) declares.
    ///
    /// Absent is no declared acknowledgement fact, which is how every
    /// model written before the semantic existed reads; migration must
    /// not guess a value the model never stated. `Some(false)` is the
    /// explicit negative. Like the outbox counterpart, the declaration
    /// is consumer-relative and neither closes an idempotency proof
    /// nor proves duplicate collapse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledge_on_success: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "schemas", rename_all = "snake_case")]
pub enum MessageSelector {
    /// Consume every message schema declared by the topic.
    All,

    /// Consume only these message schemas.
    Only(BTreeSet<Id>),
}
