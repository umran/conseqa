use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{FieldPath, Id, MessageIdentity};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataModel {
    /// Logical persistent objects belonging to this transactional
    /// state boundary.
    pub objects: BTreeMap<Id, DataObject>,

    /// Logical outboxes belonging to the same transactional state
    /// boundary: a transaction declaring this data model may mutate
    /// its objects and admit messages to its outboxes in one atomic
    /// commit. The declaration implies nothing about shared storage
    /// technology — only the atomic boundary.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outboxes: BTreeMap<Id, Outbox>,
}

/// A typed logical message collection owned by a `DataModel`.
///
/// An outbox is not a topic. A topic is a logical messaging boundary
/// written by ordinary `PublicationEffect` execution; an outbox is a
/// transactional message collection written only by an
/// `OutboxWriteEffect` inside a transaction on the owning data model,
/// whose admission is atomic with that transaction's commit. It is
/// consumed through `OutboxInput`, one logical message per logical
/// operation invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outbox {
    /// Schemas the outbox is permitted to durably contain. Membership
    /// asserts nothing about whether such a message is ever produced.
    pub messages: BTreeSet<Id>,

    /// Where the identity of one logical outbox message lives in the
    /// payload — the same semantic concept a topic declares, with the
    /// same limits: it is not a partition key, not business object
    /// identity, and implies no deduplicated writes, no at-most-once
    /// delivery, and no exactly-once processing.
    pub message_identity: MessageIdentity,
}

/// A logical class of persistent object instances.
///
/// Object-history requirements (a `linearizable` flag on the object)
/// are deliberately absent: Conseqa does not yet model the
/// replication, routing, and availability facts from which such a
/// requirement could be proved, so the DSL currently models transaction
/// and operation correctness without declaring end-to-end persistent
/// object history consistency. They are to be reconsidered, as a
/// coherent family, alongside a future model of distributed persistence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataObject {
    /// Canonical schema describing the state of this object.
    pub schema: Id,

    /// Fields defining the identity of one logical object instance.
    ///
    /// For example:
    ///
    /// Account[id]
    ///
    /// or a composite identity:
    ///
    /// TenantAccount[tenant_id, account_id]
    ///
    /// Identity is what selector precision, insert uniqueness,
    /// alias and interference analysis, locking, state-machine subject
    /// identity, and transaction reasoning rest on.
    pub identity: Vec<FieldPath>,

    /// The object's application concurrency token, when it declares
    /// one. Absent means the object carries no version token — no
    /// observed-version guard is available, though an observed-field
    /// comparison still is; never a claim that concurrent mutation is
    /// safe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<ObjectVersion>,
}

/// A versioned object's application concurrency token.
///
/// The field must be a non-optional `int` on the object's canonical
/// schema, outside the object's identity. It is managed by the
/// persistence protocol, never assigned by an application mutation,
/// and publication is an invariant of the object rather than a
/// transaction step: insertion establishes an initial token, every
/// committed transaction that mutates a live instance leaves it
/// strictly greater than before that mutation (the precise increment
/// is not observable — once per transaction, per statement, by an ORM
/// or a trigger all qualify), and deletion removes the instance.
///
/// A token is what lets one comparison guard every field an earlier
/// read observed: a [`CompareAndSet`](super::CompareAndSet) — or a
/// transition or cursor advance carrying the comparison (a fence holds
/// no write protection on an equal token, so it guards nothing) —
/// that requires the version an earlier read of the same instance
/// observed cannot succeed once any other committed transaction has
/// mutated or deleted that instance, so the stale observation cannot
/// participate in a successful commit. No writer-side annotation is
/// involved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectVersion {
    pub field: FieldPath,
}
