//! The capability vocabulary, and the needs a spec derives (§6 of the
//! realization profiles revision).
//!
//! A capability is one thing a realization can provide, written as an
//! `axis: value` pair whose value means exactly what the semantics
//! contract says, and nothing a vendor calls by the same name:
//! `isolation: serializable` is serializability of committed histories
//! (§17 of the semantics contract), which Oracle's `SERIALIZABLE` is
//! not. The vocabulary is closed and versioned by
//! [`REALIZATION_FORMAT`](super::REALIZATION_FORMAT). No value implies
//! another: an entry providing serializable isolation lists snapshot
//! isolation too, if it provides it, so that coverage is a plain
//! membership test a reviewer can check by reading the entry.
//!
//! Needs are derived, never declared (§3.1, §6.3). A data model needs
//! what its transactions ask of their store; a runtime declaration
//! needs what it declares. Every need carries the declarations it comes
//! from, so an infeasible declaration names the transaction and step
//! that asked for what no candidate provides (§10.4).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::analyzer::basis::{DeclarationKey, TextualKey};
use crate::design::workspace::WorkspaceState;
use crate::spec::{
    DataModel, DeliverySemantics, FieldPath, Id, LockMode, MemberAssignment, MemberConcurrency,
    Model, ObjectSelector, OperationBlock, OrderingSemantics, OutboxOrdering, OutboxPartitioning,
    RuntimeModel, SelectorPredicate, SelectorValue, StateMachine, Topic, Transaction,
    TransactionIsolation, TransactionStep, ValueSource,
};

/// One capability (§6.2), in the table's order, which is the order
/// every list of capabilities is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    DurabilityCommitted,
    IsolationReadCommitted,
    IsolationSnapshot,
    IsolationSerializable,
    TransactionInteractive,
    TransactionMultiInstance,
    LockShared,
    LockExclusive,
    MutationConditional,
    MutationUpsert,
    UniquenessIdentity,
    OutboxAtomic,
    ChannelFanout,
    RedriveDurable,
    GroupingKeyed,
    OrderingWithinGroup,
    OrderingGlobal,
    DeliveryAtLeastOnce,
    DeliveryAtMostOnce,
    AssignmentConsistentHash,
    AssignmentRoundRobin,
    ConcurrencyBounded,
    PartitioningKeyed,
    PartitioningComposite,
}

/// Every capability, with its axis and value as written.
const VOCABULARY: [(Capability, &str, &str); 24] = [
    (Capability::DurabilityCommitted, "durability", "committed"),
    (
        Capability::IsolationReadCommitted,
        "isolation",
        "read_committed",
    ),
    (Capability::IsolationSnapshot, "isolation", "snapshot"),
    (
        Capability::IsolationSerializable,
        "isolation",
        "serializable",
    ),
    (
        Capability::TransactionInteractive,
        "transaction",
        "interactive",
    ),
    (
        Capability::TransactionMultiInstance,
        "transaction",
        "multi_instance",
    ),
    (Capability::LockShared, "lock", "shared"),
    (Capability::LockExclusive, "lock", "exclusive"),
    (Capability::MutationConditional, "mutation", "conditional"),
    (Capability::MutationUpsert, "mutation", "upsert"),
    (Capability::UniquenessIdentity, "uniqueness", "identity"),
    (Capability::OutboxAtomic, "outbox", "atomic"),
    (Capability::ChannelFanout, "channel", "fanout"),
    (Capability::RedriveDurable, "redrive", "durable"),
    (Capability::GroupingKeyed, "grouping", "keyed"),
    (Capability::OrderingWithinGroup, "ordering", "within_group"),
    (Capability::OrderingGlobal, "ordering", "global"),
    (Capability::DeliveryAtLeastOnce, "delivery", "at_least_once"),
    (Capability::DeliveryAtMostOnce, "delivery", "at_most_once"),
    (
        Capability::AssignmentConsistentHash,
        "assignment",
        "consistent_hash",
    ),
    (
        Capability::AssignmentRoundRobin,
        "assignment",
        "round_robin",
    ),
    (Capability::ConcurrencyBounded, "concurrency", "bounded"),
    (Capability::PartitioningKeyed, "partitioning", "keyed"),
    (
        Capability::PartitioningComposite,
        "partitioning",
        "composite",
    ),
];

impl Capability {
    /// Every capability, in §6.2's order.
    pub fn every() -> impl Iterator<Item = Self> {
        VOCABULARY.iter().map(|(capability, _, _)| *capability)
    }

    fn entry(self) -> &'static (Capability, &'static str, &'static str) {
        VOCABULARY
            .iter()
            .find(|(capability, _, _)| *capability == self)
            .expect("every capability is in the vocabulary")
    }

    pub fn axis(self) -> &'static str {
        self.entry().1
    }

    pub fn value(self) -> &'static str {
        self.entry().2
    }

    /// The capability an axis and a value name, or why there is none,
    /// naming what there is instead.
    pub fn parse(axis: &str, value: &str) -> Result<Self, String> {
        let values: Vec<&(Capability, &str, &str)> = VOCABULARY
            .iter()
            .filter(|(_, named, _)| *named == axis)
            .collect();

        if values.is_empty() {
            let mut axes: Vec<&str> = VOCABULARY.iter().map(|(_, axis, _)| *axis).collect();
            axes.dedup();

            return Err(format!(
                "`{axis}` is not a capability axis; the axes are {}",
                axes.join(", ")
            ));
        }

        values
            .iter()
            .find(|(_, _, named)| *named == value)
            .map(|(capability, _, _)| *capability)
            .ok_or_else(|| {
                format!(
                    "`{axis}` has no value `{value}`; its values are {}",
                    values
                        .iter()
                        .map(|(_, _, value)| *value)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.axis(), self.value())
    }
}

/// A capability is written as a one-key map, `{ axis: value }`, in a
/// catalogue entry and in a document alike.
impl Serialize for Capability {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry(self.axis(), self.value())?;
        map.end()
    }
}

impl<'de> Deserialize<'de> for Capability {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(CapabilityVisitor)
    }
}

struct CapabilityVisitor;

impl<'de> Visitor<'de> for CapabilityVisitor {
    type Value = Capability;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a capability: one `axis: value` pair, such as `grouping: keyed`")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let Some(axis) = map.next_key::<String>()? else {
            return Err(de::Error::custom(
                "a capability is one `axis: value` pair, and this map is empty",
            ));
        };

        let value = map.next_value::<String>()?;

        if map.next_key::<String>()?.is_some() {
            return Err(de::Error::custom(
                "a capability is one `axis: value` pair; write each capability as an item of \
                 its own",
            ));
        }

        Capability::parse(&axis, &value).map_err(de::Error::custom)
    }
}

/// A kind of declaration technologies realize (§6.1), in the
/// partition's order. A data object is not one: a data model is one
/// transactional boundary, so its objects are realized by its store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RealizableKind {
    DataModel,
    Outbox,
    Topic,
    TopicRuntime,
    SubscriptionRuntime,
    OutboxRuntime,
    ExecutionPool,
    Router,
    StorageLayout,
}

impl RealizableKind {
    pub const ALL: [Self; 9] = [
        Self::DataModel,
        Self::Outbox,
        Self::Topic,
        Self::TopicRuntime,
        Self::SubscriptionRuntime,
        Self::OutboxRuntime,
        Self::ExecutionPool,
        Self::Router,
        Self::StorageLayout,
    ];

    /// The kind a declaration is, if it is realizable.
    pub fn of(declaration: &DeclarationKey) -> Option<Self> {
        Self::named(declaration.kind())
    }

    /// The kind a member kind's name names, if it is realizable.
    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }

    /// The kind, spelled as a textual key spells it.
    pub fn name(self) -> &'static str {
        match self {
            Self::DataModel => "data_model",
            Self::Outbox => "outbox",
            Self::Topic => "topic",
            Self::TopicRuntime => "topic_runtime",
            Self::SubscriptionRuntime => "subscription_runtime",
            Self::OutboxRuntime => "outbox_runtime",
            Self::ExecutionPool => "execution_pool",
            Self::Router => "router",
            Self::StorageLayout => "storage_layout",
        }
    }

    /// How many ids a declaration of the kind names: the shape of a
    /// choice file's path (§10.1).
    pub fn arity(self) -> usize {
        DeclarationKey::textual_kinds()
            .iter()
            .find(|(kind, _)| *kind == self.name())
            .map(|(_, arity)| *arity)
            .expect("every realizable kind has a textual form")
    }
}

impl fmt::Display for RealizableKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The declaration that realizes a declaration of a kind that is not
/// realizable itself: a data object's data model (§6.1).
pub fn realized_by(declaration: &DeclarationKey) -> Option<TextualKey> {
    match declaration {
        DeclarationKey::DataObject { data_model, .. } => {
            TextualKey::new(DeclarationKey::DataModel(data_model.clone()))
        }
        _ => None,
    }
}

/// One capability a declaration needs, and every place the spec asks
/// for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Need {
    pub capability: Capability,
    pub sources: Vec<Source>,
}

/// Where the spec asks for a capability: a declaration, and why.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Source {
    pub declaration: TextualKey,

    #[serde(flatten)]
    pub because: Because,
}

/// Why a declaration asks for a capability (§6.3).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "because", rename_all = "snake_case")]
pub enum Because {
    /// A data model keeps what its transactions commit (§5 of the
    /// semantics contract).
    Persists,

    /// Every subscription of a topic receives every message (§6 of the
    /// semantics contract).
    Fanout,

    /// An outbox re-drives every admitted message until it is consumed
    /// (§8.3 of the semantics contract).
    Redrive,

    /// The declaration declares a field: a transaction's isolation, or
    /// a runtime declaration's grouping, ordering, delivery, routing,
    /// concurrency, or partition key.
    Declares { field: &'static str },

    /// A step's value, comparison, or selector uses what the
    /// transaction read, so the transaction reads, decides, and writes
    /// in one commit.
    ReadsThenDecides { step: Step, source: String },

    /// The transaction writes more than one instance in its one commit.
    WritesSeveral { steps: Vec<Step> },

    /// A step asks for it: a lock, a guarded mutation, an upsert, an
    /// insert, or an outbox write.
    Step { step: Step },
}

/// One step of a transaction, as a need names it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Step {
    /// The step's position in the transaction, counted from 1.
    pub index: usize,

    pub kind: &'static str,

    /// The object the step touches, or the outbox it writes.
    pub target: Id,
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "step {} ({} {})", self.index, self.kind, self.target)
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", self.declaration)?;

        match &self.because {
            Because::Persists => f.write_str("it persists what its transactions commit"),
            Because::Fanout => f.write_str("every subscription receives every message"),
            Because::Redrive => f.write_str("every admitted message is re-driven until consumed"),
            Because::Declares { field } => write!(f, "declares `{field}`"),
            Because::ReadsThenDecides { step, source } => {
                write!(f, "{step} uses `{source}`, read in the same transaction")
            }
            Because::WritesSeveral { steps } => write!(
                f,
                "writes several instances in one commit: {}",
                steps
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Because::Step { step } => write!(f, "{step}"),
        }
    }
}

/// What needs are derived from: the parts of a spec that declare what a
/// realization must provide. It is read from an assembled model, or
/// from a workspace whose operations need not all have programs yet, as
/// a side of a diff may not (Part I §13.4): an operation without a
/// program asks nothing of its data models.
pub struct Spec<'a> {
    data_models: &'a BTreeMap<Id, DataModel>,
    topics: &'a BTreeMap<Id, Topic>,
    state_machines: &'a BTreeMap<Id, StateMachine>,
    programs: Vec<(&'a Id, &'a OperationBlock)>,
    runtime: Option<&'a RuntimeModel>,
}

impl<'a> Spec<'a> {
    pub fn of_model(model: &'a Model) -> Self {
        Self {
            data_models: &model.data_models,
            topics: &model.topics,
            state_machines: &model.state_machines,
            programs: model
                .operations
                .iter()
                .map(|(id, operation)| (id, &operation.program))
                .collect(),
            runtime: model.runtime.as_ref(),
        }
    }

    pub fn of_workspace(workspace: &'a WorkspaceState) -> Self {
        Self {
            data_models: &workspace.data_models,
            topics: &workspace.topics,
            state_machines: &workspace.state_machines,
            programs: workspace
                .operations
                .iter()
                .filter_map(|(id, draft)| Some((id, draft.program.as_ref()?)))
                .collect(),
            runtime: Some(&workspace.runtime),
        }
    }
}

/// Every realizable declaration of the model, in the partition's order,
/// with what it needs (§6.3) in the vocabulary's order. A declaration
/// needing nothing is listed with no needs.
pub fn needs(model: &Model) -> BTreeMap<TextualKey, Vec<Need>> {
    needs_of(&Spec::of_model(model))
}

/// The needs of a spec, read as [`needs`] reads a model's.
pub fn needs_of(spec: &Spec<'_>) -> BTreeMap<TextualKey, Vec<Need>> {
    let mut needs = Needs::default();

    for (data_model, declaration) in spec.data_models {
        let key = textual(DeclarationKey::DataModel(data_model.clone()));

        needs.add(
            &key,
            Capability::DurabilityCommitted,
            &key,
            Because::Persists,
        );

        for outbox in declaration.outboxes.keys() {
            let key = textual(DeclarationKey::Outbox {
                data_model: data_model.clone(),
                outbox: outbox.clone(),
            });

            needs.add(&key, Capability::RedriveDurable, &key, Because::Redrive);
        }
    }

    for topic in spec.topics.keys() {
        let key = textual(DeclarationKey::Topic(topic.clone()));

        needs.add(&key, Capability::ChannelFanout, &key, Because::Fanout);
    }

    for (operation, program) in &spec.programs {
        for (_, transaction) in program.transactions() {
            let Some(data_model) = &transaction.data_model else {
                continue;
            };

            let data_model = textual(DeclarationKey::DataModel(data_model.clone()));
            let source = textual(DeclarationKey::Transaction {
                operation: (*operation).clone(),
                transaction: transaction.id.clone(),
            });

            for (capability, because) in transaction_needs(spec, transaction) {
                needs.add(&data_model, capability, &source, because);
            }
        }
    }

    if let Some(runtime) = spec.runtime {
        for (topic, declaration) in &runtime.topics {
            let key = textual(DeclarationKey::TopicRuntime(topic.clone()));

            needs.declare(&key);
            needs.transport(&key, declaration.grouping.is_some(), declaration.ordering);
        }

        for (operation, inputs) in &runtime.subscriptions {
            for (input, declaration) in inputs {
                let key = textual(DeclarationKey::SubscriptionRuntime {
                    operation: operation.clone(),
                    input: input.clone(),
                });

                needs.declare(&key);

                let delivery = match declaration.delivery {
                    DeliverySemantics::Unspecified => None,
                    DeliverySemantics::AtLeastOnce => Some(Capability::DeliveryAtLeastOnce),
                    DeliverySemantics::AtMostOnce => Some(Capability::DeliveryAtMostOnce),
                };

                if let Some(delivery) = delivery {
                    needs.declared(&key, delivery, "delivery");
                }

                // Grouping and ordering declared here are the
                // subscription-scoped mode's; in topic-scoped mode the
                // topic runtime needs them, and this declares neither.
                needs.transport(&key, declaration.grouping.is_some(), declaration.ordering);

                if let Some(routing) = &declaration.dispatch.routing {
                    needs.declared(
                        &key,
                        assignment(routing.member_assignment),
                        "dispatch.routing",
                    );
                }
            }
        }

        for (operation, inputs) in &runtime.outboxes {
            for (input, declaration) in inputs {
                let key = textual(DeclarationKey::OutboxRuntime {
                    operation: operation.clone(),
                    input: input.clone(),
                });

                needs.declare(&key);

                if let OutboxPartitioning::Keyed(_) = declaration.partitioning {
                    needs.declared(&key, Capability::GroupingKeyed, "partitioning");
                }

                // An outbox partition is the runtime's grouping domain,
                // so precedence within one is precedence within a group.
                let ordering = match declaration.ordering {
                    OutboxOrdering::None => None,
                    OutboxOrdering::Partition => Some(Capability::OrderingWithinGroup),
                    OutboxOrdering::Global => Some(Capability::OrderingGlobal),
                };

                if let Some(ordering) = ordering {
                    needs.declared(&key, ordering, "ordering");
                }

                if let Some(routing) = &declaration.dispatch.routing {
                    needs.declared(
                        &key,
                        assignment(routing.member_assignment),
                        "dispatch.routing",
                    );
                }
            }
        }

        for (pool, declaration) in &runtime.execution_pools {
            let key = textual(DeclarationKey::ExecutionPool(pool.clone()));

            needs.declare(&key);

            if let MemberConcurrency::Bounded(_) = declaration.member_concurrency {
                needs.declared(&key, Capability::ConcurrencyBounded, "member_concurrency");
            }
        }

        for (router, declaration) in &runtime.routers {
            let key = textual(DeclarationKey::Router(router.clone()));

            needs.declare(&key);

            if let Some(routing) = &declaration.routing {
                needs.declared(&key, assignment(routing.member_assignment), "routing");
            }
        }

        for (layout, declaration) in &runtime.storage_layouts {
            let key = textual(DeclarationKey::StorageLayout(layout.clone()));

            needs.declare(&key);

            match declaration.partition_key.len() {
                0 => {}
                1 => needs.declared(&key, Capability::PartitioningKeyed, "partition_key"),
                _ => needs.declared(&key, Capability::PartitioningComposite, "partition_key"),
            }
        }
    }

    needs.into_lists()
}

/// The needs being gathered: per declaration, per capability, every
/// source, each once.
#[derive(Default)]
struct Needs(BTreeMap<TextualKey, BTreeMap<Capability, BTreeSet<Source>>>);

impl Needs {
    fn declare(&mut self, key: &TextualKey) {
        self.0.entry(key.clone()).or_default();
    }

    fn add(
        &mut self,
        key: &TextualKey,
        capability: Capability,
        declaration: &TextualKey,
        because: Because,
    ) {
        self.0
            .entry(key.clone())
            .or_default()
            .entry(capability)
            .or_default()
            .insert(Source {
                declaration: declaration.clone(),
                because,
            });
    }

    /// A need the declaration asks for by declaring a field of its own.
    fn declared(&mut self, key: &TextualKey, capability: Capability, field: &'static str) {
        self.add(key, capability, key, Because::Declares { field });
    }

    /// A topic or subscription runtime's grouping and ordering: keyed
    /// grouping when grouping is declared, and the ordering when it
    /// orders anything.
    fn transport(&mut self, key: &TextualKey, grouped: bool, ordering: Option<OrderingSemantics>) {
        if grouped {
            self.declared(key, Capability::GroupingKeyed, "grouping");
        }

        match ordering {
            Some(OrderingSemantics::WithinGroup) => {
                self.declared(key, Capability::OrderingWithinGroup, "ordering");
            }
            Some(OrderingSemantics::Global) => {
                self.declared(key, Capability::OrderingGlobal, "ordering");
            }
            Some(OrderingSemantics::None) | None => {}
        }
    }

    fn into_lists(self) -> BTreeMap<TextualKey, Vec<Need>> {
        self.0
            .into_iter()
            .map(|(key, needs)| {
                let needs = needs
                    .into_iter()
                    .map(|(capability, sources)| Need {
                        capability,
                        sources: sources.into_iter().collect(),
                    })
                    .collect();

                (key, needs)
            })
            .collect()
    }
}

fn textual(key: DeclarationKey) -> TextualKey {
    TextualKey::new(key).expect("a realizable declaration has a textual key")
}

fn assignment(assignment: MemberAssignment) -> Capability {
    match assignment {
        MemberAssignment::ConsistentHash => Capability::AssignmentConsistentHash,
        MemberAssignment::RoundRobin => Capability::AssignmentRoundRobin,
    }
}

/// What one transaction asks of its data model's store (§6.3).
fn transaction_needs(spec: &Spec<'_>, transaction: &Transaction) -> Vec<(Capability, Because)> {
    let mut needs = Vec::new();

    let isolation = match transaction.isolation {
        TransactionIsolation::Unspecified => None,
        TransactionIsolation::ReadCommitted => Some(Capability::IsolationReadCommitted),
        TransactionIsolation::Snapshot => Some(Capability::IsolationSnapshot),
        TransactionIsolation::Serializable => Some(Capability::IsolationSerializable),
    };

    if let Some(isolation) = isolation {
        needs.push((isolation, Because::Declares { field: "isolation" }));
    }

    let mut writes: Vec<(Step, Written<'_>)> = Vec::new();

    for (position, step) in transaction.steps.iter().enumerate() {
        let index = position + 1;
        let kind = step_kind(step);

        let at = |target: &Id| Step {
            index,
            kind,
            target: target.clone(),
        };

        // A value, comparison, or selector resting on what this
        // transaction observed: a read's binding, or the persistent
        // state of a state machine's subject.
        let observed = step.roots().into_iter().find(|root| {
            matches!(
                root.source,
                ValueSource::TransactionRead(_) | ValueSource::StateMachineSubject(_)
            )
        });

        if let Some(root) = observed
            && let Some(target) = step_target(step)
        {
            needs.push((
                Capability::TransactionInteractive,
                Because::ReadsThenDecides {
                    step: at(target),
                    source: format!("{}:{}", root.source.kind_name(), root.source.id()),
                },
            ));
        }

        match step {
            TransactionStep::Read(_)
            | TransactionStep::EstablishEffectIntent(_)
            | TransactionStep::EstablishTransactionOutput(_) => {}

            TransactionStep::Lock(lock) => {
                let capability = match lock.mode {
                    LockMode::Shared => Capability::LockShared,
                    LockMode::Exclusive => Capability::LockExclusive,
                };

                needs.push((
                    capability,
                    Because::Step {
                        step: at(&lock.target.object),
                    },
                ));
            }

            TransactionStep::Update(update) => {
                writes.push((
                    at(&update.target.object),
                    written(spec, transaction, &update.target),
                ));
            }

            TransactionStep::Delete(delete) => {
                writes.push((
                    at(&delete.target.object),
                    written(spec, transaction, &delete.target),
                ));
            }

            TransactionStep::CompareAndSet(cas) => {
                needs.push((
                    Capability::MutationConditional,
                    Because::Step {
                        step: at(&cas.target.object),
                    },
                ));
                writes.push((
                    at(&cas.target.object),
                    written(spec, transaction, &cas.target),
                ));
            }

            TransactionStep::AdvanceCursor(advance) => {
                needs.push((
                    Capability::MutationConditional,
                    Because::Step {
                        step: at(&advance.target.object),
                    },
                ));
                writes.push((
                    at(&advance.target.object),
                    written(spec, transaction, &advance.target),
                ));
            }

            TransactionStep::Fence(fence) => {
                needs.push((
                    Capability::MutationConditional,
                    Because::Step {
                        step: at(&fence.target.object),
                    },
                ));
                writes.push((
                    at(&fence.target.object),
                    written(spec, transaction, &fence.target),
                ));
            }

            TransactionStep::Transition(transition) => {
                needs.push((
                    Capability::MutationConditional,
                    Because::Step {
                        step: at(&transition.subject.object),
                    },
                ));
                writes.push((
                    at(&transition.subject.object),
                    written(spec, transaction, &transition.subject),
                ));

                // Each transition-scoped admission is an outbox write
                // committed with the transition (§22 of the semantics
                // contract).
                let declared = spec
                    .state_machines
                    .get(&transition.machine)
                    .and_then(|machine| machine.transitions.get(&transition.transition));

                for effect in transition.effects.keys() {
                    let Some(outbox) = declared
                        .and_then(|declared| declared.effects.get(effect))
                        .map(|declared| &declared.outbox_write().outbox)
                    else {
                        continue;
                    };

                    needs.push((Capability::OutboxAtomic, Because::Step { step: at(outbox) }));
                    writes.push((at(outbox), Written::Outbox));
                }
            }

            TransactionStep::Upsert(upsert) => {
                let step = at(&upsert.target.object);

                needs.push((
                    Capability::MutationUpsert,
                    Because::Step { step: step.clone() },
                ));
                needs.push((
                    Capability::UniquenessIdentity,
                    Because::Step { step: step.clone() },
                ));
                writes.push((step, written(spec, transaction, &upsert.target)));
            }

            TransactionStep::Insert(insert) => {
                let step = at(&insert.object);

                needs.push((
                    Capability::UniquenessIdentity,
                    Because::Step { step: step.clone() },
                ));
                writes.push((step, Written::Created));
            }

            TransactionStep::WriteOutbox(write) => {
                let step = at(&write.effect.outbox);

                needs.push((
                    Capability::OutboxAtomic,
                    Because::Step { step: step.clone() },
                ));
                writes.push((step, Written::Outbox));
            }
        }
    }

    if instances(&writes) > 1 {
        needs.push((
            Capability::TransactionMultiInstance,
            Because::WritesSeveral {
                steps: writes.into_iter().map(|(step, _)| step).collect(),
            },
        ));
    }

    needs
}

/// What one write of a transaction writes.
enum Written<'a> {
    /// The one instance a selector identifies: its object, and the
    /// value pinning each identity field.
    Identified {
        object: &'a Id,
        identity: Vec<&'a SelectorValue>,
    },

    /// Whatever instances a selector that pins no whole identity
    /// selects: possibly several.
    Several,

    /// The instance an insert creates.
    Created,

    /// One outbox message.
    Outbox,
}

/// How many instances the writes may write: an identified instance once
/// however often it is written, every created instance and outbox
/// message once each, and a selector that identifies no one instance as
/// several.
fn instances(writes: &[(Step, Written<'_>)]) -> usize {
    let mut identified: Vec<(&Id, &[&SelectorValue])> = Vec::new();
    let mut count = 0;

    for (_, write) in writes {
        match write {
            Written::Identified { object, identity } => {
                if !identified
                    .iter()
                    .any(|(seen, values)| seen == object && *values == identity.as_slice())
                {
                    identified.push((object, identity));
                    count += 1;
                }
            }
            Written::Several => count += 2,
            Written::Created | Written::Outbox => count += 1,
        }
    }

    count
}

/// What a selector of the transaction's data model writes.
fn written<'a>(
    spec: &Spec<'a>,
    transaction: &Transaction,
    selector: &'a ObjectSelector,
) -> Written<'a> {
    let identity = transaction
        .data_model
        .as_ref()
        .and_then(|data_model| spec.data_models.get(data_model))
        .and_then(|data_model| data_model.objects.get(&selector.object))
        .map(|object| object.identity.as_slice())
        .unwrap_or_default();

    let pinned = pins(&selector.predicate);

    let values: Option<Vec<&SelectorValue>> = identity
        .iter()
        .map(|field| {
            pinned
                .iter()
                .find(|(pinned, _)| *pinned == field)
                .map(|(_, value)| *value)
        })
        .collect();

    match values {
        Some(identity) if !identity.is_empty() => Written::Identified {
            object: &selector.object,
            identity,
        },
        _ => Written::Several,
    }
}

/// The fields a predicate pins by equality, with what pins each.
fn pins(predicate: &SelectorPredicate) -> Vec<(&FieldPath, &SelectorValue)> {
    match predicate {
        SelectorPredicate::All => Vec::new(),
        SelectorPredicate::Eq { field, value } => vec![(field, value)],
        SelectorPredicate::And { predicates } => predicates.iter().flat_map(pins).collect(),
    }
}

/// The step's kind, as the DSL spells it.
fn step_kind(step: &TransactionStep) -> &'static str {
    match step {
        TransactionStep::Read(_) => "read",
        TransactionStep::Update(_) => "update",
        TransactionStep::CompareAndSet(_) => "compare_and_set",
        TransactionStep::Insert(_) => "insert",
        TransactionStep::Upsert(_) => "upsert",
        TransactionStep::Delete(_) => "delete",
        TransactionStep::Lock(_) => "lock",
        TransactionStep::Transition(_) => "transition",
        TransactionStep::AdvanceCursor(_) => "advance_cursor",
        TransactionStep::Fence(_) => "fence",
        TransactionStep::EstablishEffectIntent(_) => "establish_effect_intent",
        TransactionStep::EstablishTransactionOutput(_) => "establish_transaction_output",
        TransactionStep::WriteOutbox(_) => "write_outbox",
    }
}

/// What a step touches, as a need names it: the object it selects or
/// inserts, the outbox it writes, or the effect or output it
/// establishes.
fn step_target(step: &TransactionStep) -> Option<&Id> {
    match step {
        TransactionStep::WriteOutbox(write) => Some(&write.effect.outbox),
        TransactionStep::EstablishEffectIntent(establish) => Some(&establish.effect_id),
        TransactionStep::EstablishTransactionOutput(establish) => Some(&establish.bind),
        _ => step.object(),
    }
}
