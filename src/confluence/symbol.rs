//! Semantic symbols: the units of observation, versioning, and
//! invalidation.
//!
//! An operation is deliberately not one monolithic symbol. Its
//! interface, program, requirements, and derived summary version
//! separately, so a task that read only a callee's interface is not
//! invalidated when the callee's requirements change (§9.1 of the
//! confluence spec).
//!
//! The L1 runtime declarations are symbols of their own, and every one
//! of them is shared. Runtime topology is architecture: which
//! execution population a boundary is assigned to, and how much may
//! run there, is a decision about the whole system rather than part of
//! any single operation's synthesis — the same reason a service
//! carries no topology meaning. A subscription runtime therefore names
//! an operation and an input without belonging to that operation's
//! write authority.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::spec::Id;

use super::fingerprint::SemanticHash;
use super::workspace::PromptObligationId;

/// The five requirement families verification discharges. Result
/// replay is the result half of an idempotency declaration, split out
/// because it is proven separately.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum RequirementFamily {
    Serialization,
    Ordering,
    Idempotency,
    ResultReplay,
    Recoverability,
}

impl fmt::Display for RequirementFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Serialization => "serialization",
            Self::Ordering => "ordering",
            Self::Idempotency => "idempotency",
            Self::ResultReplay => "result_replay",
            Self::Recoverability => "recoverability",
        })
    }
}

/// Identity of one tracked semantic symbol.
///
/// Program-local symbols — transactions, effect sites, bindings — use
/// the stable logical IDs the DSL already gives them; positional
/// `StepLocation` is never promoted to durable semantic identity.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum SymbolKey {
    Service(Id),
    Schema(Id),

    DataModel(Id),
    DataObject {
        data_model: Id,
        object: Id,
    },

    Topic(Id),

    StateMachine(Id),
    Transition {
        machine: Id,
        transition: Id,
    },

    Operation(Id),

    OperationInterface(Id),
    OperationProgram(Id),
    OperationRequirements(Id),

    Input {
        operation: Id,
        input: Id,
    },

    Transaction {
        operation: Id,
        transaction: Id,
    },

    EffectSite {
        operation: Id,
        effect: Id,
    },

    Binding {
        operation: Id,
        binding: Id,
    },

    /// One declared requirement. The fingerprint is the requirement's
    /// content hash; `occurrence` disambiguates identical declarations
    /// within the same operation and family, in declaration order.
    Requirement {
        operation: Id,
        family: RequirementFamily,
        fingerprint: SemanticHash,
        occurrence: u32,
    },

    // ---- L1: runtime topology ----
    /// Transport facts for one topic.
    TopicRuntime(Id),

    /// Delivery and dispatch facts for one subscription boundary.
    SubscriptionRuntime {
        operation: Id,
        input: Id,
    },

    ExecutionPool(Id),
    Router(Id),
    StorageLayout(Id),

    /// The derived proof summary of an operation. Not stored in the
    /// workspace — it is analysis output — but addressable for reads,
    /// with observation tracked against the summary's inputs.
    OperationSummary(Id),

    PromptObligation(PromptObligationId),
}

impl SymbolKey {
    pub fn kind(&self) -> SymbolKind {
        match self {
            Self::Service(_) => SymbolKind::Service,
            Self::Schema(_) => SymbolKind::Schema,
            Self::DataModel(_) => SymbolKind::DataModel,
            Self::DataObject { .. } => SymbolKind::DataObject,
            Self::Topic(_) => SymbolKind::Topic,
            Self::StateMachine(_) => SymbolKind::StateMachine,
            Self::Transition { .. } => SymbolKind::Transition,
            Self::Operation(_) => SymbolKind::Operation,
            Self::OperationInterface(_) => SymbolKind::OperationInterface,
            Self::OperationProgram(_) => SymbolKind::OperationProgram,
            Self::OperationRequirements(_) => SymbolKind::OperationRequirements,
            Self::TopicRuntime(_) => SymbolKind::TopicRuntime,
            Self::SubscriptionRuntime { .. } => SymbolKind::SubscriptionRuntime,
            Self::ExecutionPool(_) => SymbolKind::ExecutionPool,
            Self::Router(_) => SymbolKind::Router,
            Self::StorageLayout(_) => SymbolKind::StorageLayout,
            Self::Input { .. } => SymbolKind::Input,
            Self::Transaction { .. } => SymbolKind::Transaction,
            Self::EffectSite { .. } => SymbolKind::EffectSite,
            Self::Binding { .. } => SymbolKind::Binding,
            Self::Requirement { .. } => SymbolKind::Requirement,
            Self::OperationSummary(_) => SymbolKind::OperationSummary,
            Self::PromptObligation(_) => SymbolKind::PromptObligation,
        }
    }

    /// Which authority owns writes to the symbol: an operation-scoped
    /// symbol belongs to its operation, everything else to the shared
    /// skeleton.
    pub fn owner(&self) -> SymbolOwner {
        match self {
            Self::Operation(id)
            | Self::OperationInterface(id)
            | Self::OperationProgram(id)
            | Self::OperationRequirements(id)
            | Self::OperationSummary(id) => SymbolOwner::Operation(id.clone()),

            Self::Input { operation, .. }
            | Self::Transaction { operation, .. }
            | Self::EffectSite { operation, .. }
            | Self::Binding { operation, .. }
            | Self::Requirement { operation, .. } => SymbolOwner::Operation(operation.clone()),

            _ => SymbolOwner::Shared,
        }
    }

    /// The operation an operation-scoped key belongs to.
    pub fn operation(&self) -> Option<&Id> {
        match self {
            Self::Operation(id)
            | Self::OperationInterface(id)
            | Self::OperationProgram(id)
            | Self::OperationRequirements(id)
            | Self::OperationSummary(id) => Some(id),

            Self::Input { operation, .. }
            | Self::Transaction { operation, .. }
            | Self::EffectSite { operation, .. }
            | Self::Binding { operation, .. }
            | Self::Requirement { operation, .. } => Some(operation),

            _ => None,
        }
    }
}

impl fmt::Display for SymbolKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Service(id) => write!(f, "service({id})"),
            Self::Schema(id) => write!(f, "schema({id})"),
            Self::DataModel(id) => write!(f, "data_model({id})"),
            Self::DataObject { data_model, object } => {
                write!(f, "data_object({data_model}/{object})")
            }
            Self::Topic(id) => write!(f, "topic({id})"),
            Self::StateMachine(id) => write!(f, "state_machine({id})"),
            Self::Transition {
                machine,
                transition,
            } => write!(f, "transition({machine}/{transition})"),
            Self::Operation(id) => write!(f, "operation({id})"),
            Self::OperationInterface(id) => write!(f, "operation_interface({id})"),
            Self::OperationProgram(id) => write!(f, "operation_program({id})"),
            Self::OperationRequirements(id) => write!(f, "operation_requirements({id})"),
            Self::TopicRuntime(id) => write!(f, "topic_runtime({id})"),
            Self::SubscriptionRuntime { operation, input } => {
                write!(f, "subscription_runtime({operation}/{input})")
            }
            Self::ExecutionPool(id) => write!(f, "execution_pool({id})"),
            Self::Router(id) => write!(f, "router({id})"),
            Self::StorageLayout(id) => write!(f, "storage_layout({id})"),
            Self::Input { operation, input } => write!(f, "input({operation}/{input})"),
            Self::Transaction {
                operation,
                transaction,
            } => write!(f, "transaction({operation}/{transaction})"),
            Self::EffectSite { operation, effect } => {
                write!(f, "effect_site({operation}/{effect})")
            }
            Self::Binding { operation, binding } => {
                write!(f, "binding({operation}/{binding})")
            }
            Self::Requirement {
                operation,
                family,
                fingerprint,
                occurrence,
            } => write!(
                f,
                "requirement({operation}/{family}/{fingerprint}#{occurrence})"
            ),
            Self::OperationSummary(id) => write!(f, "operation_summary({id})"),
            Self::PromptObligation(id) => write!(f, "prompt_obligation({id})"),
        }
    }
}

/// The kind of a symbol, as node metadata.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Service,
    Schema,
    DataModel,
    DataObject,
    Topic,
    StateMachine,
    Transition,
    Operation,
    OperationInterface,
    OperationProgram,
    OperationRequirements,
    TopicRuntime,
    SubscriptionRuntime,
    ExecutionPool,
    Router,
    StorageLayout,
    Input,
    Transaction,
    EffectSite,
    Binding,
    Requirement,
    OperationSummary,
    PromptObligation,
}

/// Write authority over a symbol.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum SymbolOwner {
    /// Part of the shared skeleton: services, schemas, data models,
    /// topics, state machines, the whole runtime topology, and prompt
    /// obligations.
    Shared,

    /// Owned by one operation's synthesis authority.
    Operation(Id),
}

/// Monotonic per-symbol version. Bumped exactly when the symbol's
/// semantic fingerprint changes; primarily diagnostics and a fast
/// short-circuit — the fingerprint comparison is authoritative.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct SymbolVersion(pub u64);

impl SymbolVersion {
    pub fn first() -> Self {
        Self(1)
    }

    pub fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl fmt::Display for SymbolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.0)
    }
}
