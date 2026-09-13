use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::spec::{FieldPath, Id, OutboxWriteEffect, PublicationEffect, RequestEffect};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateMachine {
    pub subject: StateMachineSubject,

    pub states: BTreeSet<Id>,
    pub initial: Id,

    pub transitions: BTreeMap<Id, Transition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StateMachineSubject {
    Object {
        object: Id,

        /// Field on the object's canonical schema containing
        /// the machine state.
        state: FieldPath,
    },
}

/// One transition of a state machine: an explicitly fallible commit
/// guard over the subject's state field.
///
/// Applying the transition inside a transaction means:
///
/// ```text
/// if current state ∈ from:
///     the transition may continue normally
/// otherwise:
///     the containing transaction rejects
/// ```
///
/// A rejected transition mutates nothing, establishes no transition
/// artifact, admits none of its transition-scoped outbox writes, and
/// causes the containing transaction to reject as a whole — control
/// enters the transaction step's `rejected` block. The transition
/// itself never returns an operation `Err`; the surrounding operation
/// chooses the boundary result in its rejection branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transition {
    pub from: BTreeSet<Id>,
    pub to: Id,

    pub side_effects: BTreeMap<Id, TransitionSideEffect>,

    /// Outbox messages admitted atomically with a successful
    /// application of this transition — the transition-scoped atomic
    /// admission of §15 of the DSL v4 revision.
    ///
    /// Keyed by effect id, exactly as `side_effects` is: the effect id
    /// is the stable identity of the admission site (lineage,
    /// diagnostics, proof evidence), and the applying transaction step
    /// supplies the message derivation under the same key. The
    /// declared kinds are deliberately narrow: an admission is a
    /// durable commit artifact, so no effect that executes outside the
    /// transaction — a publication, a request, an external effect, an
    /// intent execution — can be a transition effect.
    #[serde(default)]
    pub effects: BTreeMap<Id, TransitionEffect>,
}

/// An effect associated with taking a transition.
///
/// A side effect is not executed inside the application-state
/// transaction. Each transaction application of the transition
/// supplies, for every side effect, the concrete instance derivation
/// and an operation-local intent binding; a successful transition
/// establishes the bound `EffectIntent` artifact, subject to the same
/// retention and recovery rules as an explicitly established intent.
/// The operation executes it with `ExecuteEffectIntent`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TransitionSideEffect {
    Publication(PublicationEffect),
    Request(RequestEffect),
}

/// An effect admitted atomically with the transition's containing
/// transaction, conditioned on the transition itself applying.
///
/// The one kind is a transactional outbox write: successful
/// application of the transition admits the message in the same
/// commit, rejection admits nothing, and rollback or interruption of
/// the containing transaction commits nothing. It means "atomically
/// persist an outbox message", never "execute the consumer", "publish
/// externally", "call another operation", or "perform remote I/O" —
/// which is why the enum admits no other kind. The destination outbox
/// must belong to the data model that owns the machine's subject
/// object, and must admit the written schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TransitionEffect {
    OutboxWrite(OutboxWriteEffect),
}

impl TransitionEffect {
    /// The outbox-write contract of the effect.
    pub fn outbox_write(&self) -> &OutboxWriteEffect {
        match self {
            Self::OutboxWrite(write) => write,
        }
    }
}
