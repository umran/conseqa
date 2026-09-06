//! The typed semantic graph over one workspace revision.
//!
//! A dense custom representation rather than a generic graph
//! framework: node kinds, edge kinds, and the hot queries are all
//! known, and the graph is rebuilt whole after every accepted commit
//! (§14 of the confluence spec), so simplicity beats generality.

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use crate::spec::{FieldPath, Id, Revision};

use super::fingerprint::SemanticHash;
use super::symbol::{SymbolKey, SymbolKind, SymbolOwner, SymbolVersion};

/// Index of a node within one graph build. Never durable identity —
/// that is the node's [`SymbolKey`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

impl NodeId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// One tracked symbol at one revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolNode {
    pub key: SymbolKey,
    pub version: SymbolVersion,
    pub fingerprint: SemanticHash,
    pub kind: SymbolKind,
    pub owner: SymbolOwner,
}

/// The relationship kinds of the semantic graph. Deliberately not
/// collapsed into one `DependsOn`: the kind carries query semantics,
/// impact analysis, context slicing, and explanations.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Contains,

    References,

    CallsOperation,

    PublishesTopic,
    ConsumesTopic,

    ReadsObject,
    WritesObject,

    AppliesStateMachine,
    AppliesTransition,

    ProducesBinding,
    UsesBinding,

    ValueDependsOn,

    TriggeredBy,

    PropagatesIdempotencyKey,

    RequirementTargets,

    ContractDependsOn,

    ProofDependsOn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    pub kind: EdgeKind,
    pub to: NodeId,
}

/// The full graph at one revision: nodes, adjacency in both
/// directions, and the specialized indexes hot queries read directly.
#[derive(Debug, Clone)]
pub struct SymbolGraph {
    pub revision: Revision,

    pub nodes: Vec<SymbolNode>,
    pub node_ids: FxHashMap<SymbolKey, NodeId>,

    pub outgoing: Vec<SmallVec<[Edge; 4]>>,
    pub incoming: Vec<SmallVec<[Edge; 4]>>,

    pub indexes: GraphIndexes,
}

impl SymbolGraph {
    pub fn node_id(&self, key: &SymbolKey) -> Option<NodeId> {
        self.node_ids.get(key).copied()
    }

    pub fn node(&self, key: &SymbolKey) -> Option<&SymbolNode> {
        self.node_id(key).map(|id| &self.nodes[id.index()])
    }

    pub fn node_at(&self, id: NodeId) -> &SymbolNode {
        &self.nodes[id.index()]
    }

    pub fn outgoing_of(&self, id: NodeId) -> &[Edge] {
        &self.outgoing[id.index()]
    }

    pub fn incoming_of(&self, id: NodeId) -> &[Edge] {
        &self.incoming[id.index()]
    }

    /// The fingerprint of a symbol, if it exists at this revision.
    pub fn fingerprint(&self, key: &SymbolKey) -> Option<SemanticHash> {
        self.node(key).map(|node| node.fingerprint)
    }
}

/// Direct indexes for the queries agents make repeatedly, avoiding
/// generic traversal on the hot path. Id-keyed, so they tolerate
/// references to symbols that are planned but not yet declared.
#[derive(Debug, Clone, Default)]
pub struct GraphIndexes {
    /// Target operation → call edges reaching it.
    pub callers: FxHashMap<Id, Vec<CallEdge>>,

    /// Calling operation → call edges leaving it.
    pub callees: FxHashMap<Id, Vec<CallEdge>>,

    pub object_readers: FxHashMap<ObjectPathKey, Vec<ObjectAccess>>,
    pub object_writers: FxHashMap<ObjectPathKey, Vec<ObjectAccess>>,

    pub topic_publishers: FxHashMap<Id, Vec<PublisherRef>>,
    pub topic_consumers: FxHashMap<Id, Vec<ConsumerRef>>,

    pub transition_users: FxHashMap<TransitionKey, Vec<TransactionRef>>,
}

/// One operation-to-operation request relationship, with the site it
/// happens through.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallEdge {
    pub caller: Id,
    pub target: Id,
    pub target_input: Id,
    pub site: EffectRef,
}

/// A logical effect site: declared inline by an operation program, or
/// declared on a state-machine transition and executed by whichever
/// operation applies it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EffectRef {
    Operation {
        operation: Id,
        effect: Id,
    },
    Transition {
        machine: Id,
        transition: Id,
        effect: Id,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublisherRef {
    pub site: EffectRef,
    pub schema: Id,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsumerRef {
    pub operation: Id,
    pub input: Id,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionRef {
    pub operation: Id,
    pub transaction: Id,
}

/// One transaction's access to a persistent object, with the fields it
/// touches.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectAccess {
    pub transaction: TransactionRef,
    pub access: FieldAccess,
}

/// The fields an access touches. `All` covers whole-object accesses:
/// unrestricted reads, inserts, deletes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "fields", rename_all = "snake_case")]
pub enum FieldAccess {
    All,
    Fields(Vec<FieldPath>),
}

impl FieldAccess {
    /// Whether the access can touch `field`, treating path containment
    /// in either direction as overlap: an access to `customer` touches
    /// `customer.id`, and an access to `customer.id` touches
    /// `customer`.
    pub fn touches(&self, field: &FieldPath) -> bool {
        match self {
            Self::All => true,

            Self::Fields(fields) => fields.iter().any(|access| {
                let shorter = access.0.len().min(field.0.len());

                access.0[..shorter] == field.0[..shorter]
            }),
        }
    }
}

/// Identifies one persistent object class.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectPathKey {
    pub data_model: Id,
    pub object: Id,
}

/// Identifies one state-machine transition.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransitionKey {
    pub machine: Id,
    pub transition: Id,
}
