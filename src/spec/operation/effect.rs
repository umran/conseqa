use serde::{Deserialize, Serialize};

use crate::spec::{Id, IdempotencyGuarantee, IdempotencyKeyPropagation, ResultType};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Effect {
    Publication(PublicationEffect),
    Request(RequestEffect),
    External(ExternalEffect),
}

/// Publishes one schema to one topic. A publication has no synchronous
/// result and cannot bind one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationEffect {
    pub topic: Id,
    pub schema: Id,

    pub idempotency_key_propagation: Vec<IdempotencyKeyPropagation>,
}

/// Invokes a specific request input of another operation.
///
/// Its synchronous result contract is inherited from the targeted
/// input's declared `result`; it is never redeclared here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestEffect {
    pub target: RequestTarget,
    pub schema: Id,
    pub retry: RetrySemantics,

    pub idempotency_key_propagation: Vec<IdempotencyKeyPropagation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestTarget {
    pub operation: Id,
    pub input: Id,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalEffect {
    pub name: String,

    /// This is declared because the modeled system ends here;
    /// the checker cannot inspect the external implementation.
    pub idempotency: IdempotencyGuarantee,

    /// The synchronous result the boundary returns, declared for the
    /// same reason: Conseqa cannot inspect beyond it. `None` means no
    /// synchronous result is modeled.
    ///
    /// The contract declares the result's shape and the error's
    /// disposition. For a result-bearing effect, `deduplicated_by`
    /// additionally fixes the interaction's terminal logical result:
    /// equal evaluated keys identify one logical external execution,
    /// and after its first terminal `Ok` or terminal `Err`, every
    /// subsequent same-key execution observes the same variant and a
    /// replay-equivalent payload. A retryable `Err` is an attempt-level,
    /// nonterminal outcome and establishes no terminal result. The
    /// checker cannot prove the real boundary honors this; the
    /// declaration is a conformance obligation on the boundary.
    pub result: Option<ResultType>,
}

impl Effect {
    /// Whether this effect kind may be launched through
    /// `execute_effect_async`.
    ///
    /// Exactly the kinds legal for ordinary direct execution are
    /// async-capable today. The match is deliberately exhaustive: a
    /// new effect kind must decide here whether direct asynchronous
    /// execution is legal, rather than becoming async-capable merely
    /// by joining the enum.
    pub fn permits_direct_async(&self) -> bool {
        match self {
            Self::Publication(_) | Self::Request(_) | Self::External(_) => true,
        }
    }

    /// Every value reference the effect's declaration evaluates when
    /// the effect executes: an external deduplication key, and the
    /// source and target of each propagation.
    pub fn roots(&self) -> Vec<&crate::spec::ValueRef> {
        let mut roots = Vec::new();

        let propagations = match self {
            Self::Publication(effect) => &effect.idempotency_key_propagation,
            Self::Request(effect) => &effect.idempotency_key_propagation,
            Self::External(effect) => {
                if let IdempotencyGuarantee::DeduplicatedBy { key } = &effect.idempotency {
                    roots.extend(key.components.iter());
                }

                return roots;
            }
        };

        for propagation in propagations {
            roots.extend(propagation.source.components.iter());
            roots.extend(propagation.target.components.iter());
        }

        roots
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrySemantics {
    Unspecified,
    Never,
    MayRepeat,
}
