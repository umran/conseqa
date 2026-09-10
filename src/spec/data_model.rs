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
}
