use serde::{Deserialize, Serialize};

use crate::spec::Id;

/// The outbox's one logical consumer boundary: a committed message
/// admitted to the outbox may invoke this operation, and the
/// invocation's payload is that one logical message.
///
/// Exactly one `OutboxInput` in the model references a given outbox —
/// a structural invariant, not a convention. The owning operation is
/// the outbox's exclusive logical consumer: it consumes every message
/// schema the outbox admits (there is no per-input message selection),
/// and competing consumer operations are not permitted. Downstream
/// fan-out belongs to topics, not outboxes. An outbox admitting
/// heterogeneous schemas remains valid; the owning operation must be
/// capable of handling every admitted input variant.
///
/// Consumption semantics are intrinsic to the outbox abstraction
/// rather than declared: a committed message is durably pending, the
/// runtime continues to admit consumption attempts for a pending
/// message, successful logical completion of an attempt consumes it,
/// and a failed or uncertain attempt leaves it pending. `Consumed(M)`
/// only ends the admission of further ordinary attempts — it does not
/// imply that previously admitted attempts have terminated, so
/// multiple attempts for one logical message may be simultaneously
/// active (after timeout, lease expiry, or uncertainty), which is why
/// consumer idempotency remains necessary. There is no configurable
/// acknowledgement or delivery fact, and no acknowledgement statement
/// exists in the program.
///
/// The L0 program stays a per-message logical machine — no batch
/// payload or batch iteration exists. How consumption attempts are
/// partitioned, ordered, routed, and batched at runtime are
/// realization facts declared by
/// [`OutboxRuntime`](crate::spec::OutboxRuntime) against the
/// `(operation, input)` pair this input already establishes. An
/// outbox input has no synchronous result; its normal terminal is
/// `complete`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutboxInput {
    /// Outbox this input exclusively consumes.
    pub outbox: Id,
}
