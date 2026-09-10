use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{FieldPath, Id};

/// A logical message channel: which messages it carries and what
/// identifies one of them.
///
/// Both facts are L0 — they define the channel itself, not how a
/// particular runtime realizes it. The precedence a transport
/// establishes among these messages is a realization fact and lives in
/// [`TopicRuntime`](crate::spec::TopicRuntime).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Topic {
    /// Schemas that may be published to this topic.
    pub messages: BTreeSet<Id>,

    /// Where the identity of one logical message lives in the payload.
    pub message_identity: MessageIdentity,
}

/// Identity of one logical message among the topic's carried messages.
///
/// This is an implementation guarantee, deliberately distinct from the
/// ordering key: the ordering key sequences messages, the message
/// identity identifies one logical message. They may coincide, and
/// neither implies the other. It is also distinct from object
/// identity: `order_id` identifies the order, not the message about
/// the order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageIdentity {
    /// No fact relates two carried messages sharing any field values.
    Unspecified,

    /// For each mapped message schema, the ordered fields holding that
    /// schema's message identity. As with the ordering key, different
    /// schemas may map differently named fields into the same identity
    /// domain; tuple positions correspond across schemas, so all
    /// mapped tuples must have the same arity.
    ///
    /// The guarantee is one statement over the mapped population: any
    /// two messages carried by the topic, each of a mapped schema,
    /// whose identity tuples are equal are the same logical message —
    /// hence of the same schema, with equal payloads.
    ///
    /// Two publications sharing an identity are attempts at publishing
    /// one logical message; how often that message is delivered
    /// remains the subscription's delivery semantics. The mapping may
    /// cover a subset of the carried schemas — identity is meaningful
    /// knowledge per schema, unlike the ordering key, which must route
    /// every carried message.
    Keyed(MessageIdentityKey),
}

/// The per-schema identity mapping.
///
/// A named struct rather than an inline variant body so that
/// `deny_unknown_fields` applies: serde cannot enforce it on an
/// internally tagged enum, and without it a field removed from `Topic`
/// — `ordering` was this one's sibling — is silently swallowed when an
/// author nests it here while migrating.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageIdentityKey {
    pub mapping: BTreeMap<Id, Vec<FieldPath>>,
}
