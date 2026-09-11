use serde::{Deserialize, Serialize};

use crate::spec::{Id, IdempotencyKeyPropagation, ResultType, ValueRef};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Effect {
    Publication(PublicationEffect),
    Request(RequestEffect),
    External(ExternalEffect),

    /// Transactional admission of a message to a `DataModel` outbox.
    ///
    /// A real effect — it participates in the operation's side-effect
    /// blast radius, idempotency analysis, and value lineage — with
    /// exactly one legal execution site: a transaction's
    /// `write_outbox` step. Validation rejects it under
    /// `execute_effect`, `execute_effect_async`,
    /// `establish_effect_intent`, and transition side effects; the
    /// variant exists here so those illegal sites are rejected with a
    /// precise diagnostic rather than a parse error.
    OutboxWrite(OutboxWriteEffect),
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

/// Admits one logical message of one schema to a `DataModel` outbox,
/// atomically with the containing transaction's commit.
///
/// The destination outbox must belong to the transaction's declared
/// `data_model` — Conseqa never infers a distributed cross-data-model
/// atomic transaction. Like a publication, the effect has no
/// synchronous result and cannot bind one; the transaction alone
/// determines whether the staged write commits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutboxWriteEffect {
    /// The destination outbox, owned by the transaction's data model.
    pub outbox: Id,

    /// Schema of the admitted message.
    pub schema: Id,

    /// Lineage only, with exactly the meaning it has on a
    /// publication: the declared target fields of the emitted message
    /// carry the same logical idempotency identity as the declared
    /// source values. It deduplicates nothing.
    pub idempotency_key_propagation: Vec<IdempotencyKeyPropagation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestTarget {
    pub operation: Id,
    pub input: Id,
}

/// An external boundary: the modeled system ends here, so its facts
/// are declared, never proven — each is a conformance obligation on
/// the boundary (§1.3), and the checker consumes them independently.
///
/// Three orthogonal dimensions, none derivable from another:
/// `identity` (what makes two applications the same logical
/// interaction), `idempotency` (what duplicate applications do to
/// external state), and `result_replay` (what duplicate applications
/// observe as the synchronous result).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalEffect {
    pub name: String,

    /// What identifies one logical external interaction: equal
    /// evaluated key tuples are applications of the same interaction.
    /// Identity alone claims nothing about behaviour — not
    /// deduplication, not idempotency, not result replay.
    pub identity: ExternalIdentity,

    /// Duplicate-side-effect behaviour, relative to that identity.
    pub idempotency: ExternalIdempotency,

    /// Terminal-result replay behaviour, relative to that identity.
    pub result_replay: ExternalResultReplay,

    /// The synchronous result the boundary returns. `None` means no
    /// synchronous result is modeled, and no `result_replay` fact
    /// beyond `unspecified` may be declared.
    pub result: Option<ResultType>,
}

/// The boundary's notion of "one logical interaction", mirroring the
/// *shape* of request identity and message identity while remaining
/// its own vocabulary: an interaction identity is not an idempotency
/// declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExternalIdentity {
    /// No usable sameness relation across applications.
    Unspecified,

    /// Equal evaluated key tuples identify applications of one
    /// logical external interaction.
    Keyed { key: ExternalIdentityKey },
}

/// The evaluated identity of one logical external interaction.
///
/// Deliberately distinct from the public `IdempotencyKey` type even
/// though the shapes coincide: verifier internals are shared, the
/// public semantic concepts are not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalIdentityKey {
    pub components: Vec<ValueRef>,
}

/// What duplicate applications of the boundary do to modeled
/// externally observable state. A declared implementation guarantee
/// (§1.3); the DSL states the property, never the mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalIdempotency {
    /// No usable duplicate-side-effect fact. Epistemic: not "safe",
    /// not "unsafe".
    Unspecified,

    /// An explicit negative: repeated applications may produce
    /// distinguishable modeled external work (an unkeyed charge).
    Distinguishable,

    /// Across applications of one keyed interaction, any number of
    /// applications produces modeled externally observable side-effect
    /// work indistinguishable from exactly one application, under
    /// every admitted interleaving. Requires a keyed identity.
    IdenticalPerIdentity,

    /// Application causes no modeled externally observable state
    /// change beyond producing its synchronous result. Universal and
    /// keyless; unmodeled internal activity (logging, metrics,
    /// caching) is not prohibited.
    SideEffectFree,
}

/// What duplicate applications observe as the boundary's terminal
/// synchronous result. Independent of `ExternalIdempotency` in both
/// directions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalResultReplay {
    /// No usable result-replay fact.
    Unspecified,

    /// An explicit negative: per-attempt results may differ (fresh
    /// presigned URLs, fresh nonces). Requires a result contract.
    Unstable,

    /// After one keyed interaction's first terminal outcome, every
    /// later application of that identity observes the same terminal
    /// variant and a replay-equivalent payload. `Ok` is terminal by
    /// definition; `Err` only under a declared `terminal` disposition.
    /// Requires a keyed identity and a result contract.
    ReplayStable,
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

            // Transaction-exclusive: admission is atomic with the
            // containing transaction's commit, and a direct launch has
            // no containing transaction.
            Self::OutboxWrite(_) => false,
        }
    }

    /// Every value reference the effect's declaration evaluates when
    /// the effect executes: an external interaction-identity key, and
    /// the source and target of each propagation.
    pub fn roots(&self) -> Vec<&crate::spec::ValueRef> {
        let mut roots = Vec::new();

        let propagations = match self {
            Self::Publication(effect) => &effect.idempotency_key_propagation,
            Self::Request(effect) => &effect.idempotency_key_propagation,
            Self::OutboxWrite(effect) => &effect.idempotency_key_propagation,
            Self::External(effect) => {
                if let ExternalIdentity::Keyed { key } = &effect.identity {
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
