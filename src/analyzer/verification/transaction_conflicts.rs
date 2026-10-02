//! Model-wide transaction conflict analysis: the shared machinery of
//! the transaction serializability and ordering provers (§35–§52 of
//! the DSL v4 revision).
//!
//! A serializability obligation is a system-wide property over
//! potentially conflicting transaction executions, so no prover may
//! examine the declaring transaction alone. This module derives, once
//! per model:
//!
//! - **the access index** — every persistent-state access of every
//!   inline transaction template, with its object, selector domain,
//!   field footprint, and mode, including the intrinsic version
//!   publication of every mutation of a live versioned instance; lock
//!   declarations, atomic conditional mutations (compare-and-set,
//!   transition, cursor advance, fence, upsert arbitration), and commit
//!   artifacts (outbox admissions, intent establishments, outputs) are
//!   indexed beside it and never counted as conflict accesses;
//! - **selector and field overlap** — two accesses conflict only where
//!   their selected domains may overlap and their fields may overlap;
//!   unknown overlap is never treated as disjoint;
//! - **the conflict closure** — the connected component of the
//!   undirected potential-conflict graph a requirement's transaction
//!   belongs to, deliberately transitive: a serialization cycle can
//!   pass through a transaction that never touches the root's state
//!   directly;
//! - **the potential serialization-dependency graph** — directed
//!   write-read, read-write (anti-) and write-write dependencies between
//!   templates of a closure, each classified as commit-order
//!   constrained by declared evidence or not; and
//! - **the cyclic strongly connected components** of that graph, which
//!   is where an unconstrained dependency becomes a refusal.
//!
//! Everything here is conservative (§70): an unknown selector overlap
//! is potentially overlapping, an unknown field footprint potentially
//! conflicting, an unspecified isolation no isolation, and a guard that
//! does not identify one instance, or does not compare what the
//! conflict touches, no credit at all. One inline
//! declaration is one template, and concurrent executions of the same
//! template are analyzed as two, so a template may conflict with
//! itself.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::spec::{
    CompareCondition, CursorAdvanceRule, DataObject, FieldPath, FieldSelection, Id, Input, Literal,
    LockMode, MessageSelector, Model, ObjectSelector, Operation, SelectorPredicate, SelectorValue,
    StateMachineSubject, StepLocation, Transaction, TransactionIsolation, TransactionStep,
    ValueRef, ValueSource,
};

use super::value_identity::canonical_value_path;

/// One inline transaction template: the declaring operation, the
/// transaction's stable id, and where the execution site sits in the
/// program. Concurrent executions of one template are analyzed as
/// distinct transactions.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionRef {
    pub operation: Id,
    pub transaction: Id,
    pub location: StepLocation,
}

impl fmt::Display for TransactionRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.transaction)
    }
}

/// A managed field of one object: a version, cursor, or fence field.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedFieldRef {
    pub object: Id,
    pub field: FieldPath,
}

impl fmt::Display for ManagedFieldRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.object, self.field)
    }
}

/// What one transaction step does to persistent state (§37).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    Read,
    Update,

    /// The fields a guarded mutation's comparisons read, atomically
    /// with its mutation.
    CompareRead,

    /// The fields a compare-and-set mutates.
    CompareWrite,

    Insert,

    /// An upsert's identity arbitration and either branch's mutation,
    /// one atomic access.
    UpsertReadWrite,

    Delete,
    TransitionRead,
    TransitionWrite,
    CursorReadWrite,
    FenceReadWrite,

    /// The intrinsic publication of a newer version token by a
    /// mutation of a live versioned instance — not a step of its own,
    /// but a write of the version field every such mutation performs.
    VersionPublish,
}

impl AccessMode {
    /// Whether the access observes state.
    pub fn reads(self) -> bool {
        matches!(
            self,
            Self::Read
                | Self::CompareRead
                | Self::UpsertReadWrite
                | Self::TransitionRead
                | Self::CursorReadWrite
                | Self::FenceReadWrite
        )
    }

    /// Whether the access mutates state.
    pub fn writes(self) -> bool {
        matches!(
            self,
            Self::Update
                | Self::CompareWrite
                | Self::Insert
                | Self::UpsertReadWrite
                | Self::Delete
                | Self::TransitionWrite
                | Self::CursorReadWrite
                | Self::FenceReadWrite
                | Self::VersionPublish
        )
    }
}

impl fmt::Display for AccessMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Read => "read",
            Self::Update => "update",
            Self::CompareRead => "comparison",
            Self::CompareWrite => "compare-and-set",
            Self::Insert => "insert",
            Self::UpsertReadWrite => "upsert",
            Self::Delete => "delete",
            Self::TransitionRead => "transition read",
            Self::TransitionWrite => "transition write",
            Self::CursorReadWrite => "cursor advance",
            Self::FenceReadWrite => "fence",
            Self::VersionPublish => "version publication",
        })
    }
}

/// The field footprint of one access (§38).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "fields", rename_all = "snake_case")]
pub enum AccessFields {
    /// Every field of the selected instances: a read of all fields, an
    /// insert, a delete.
    All,

    Only(BTreeSet<FieldPath>),

    /// The footprint is not declared — an update naming no fields — so
    /// its provenance is unknown and it is treated as potentially
    /// conflicting with everything.
    Unknown,
}

impl fmt::Display for AccessFields {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::All => f.write_str("every field"),
            Self::Unknown => f.write_str("an undeclared footprint"),
            Self::Only(fields) => f.write_str(
                &fields
                    .iter()
                    .map(|field| format!("`{field}`"))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        }
    }
}

/// One persistent-state access of one transaction template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionAccess {
    pub transaction: TransactionRef,

    /// Index of the step in the transaction body.
    pub step: usize,

    pub object: Id,
    pub selector: ObjectSelector,
    pub fields: AccessFields,
    pub mode: AccessMode,

    /// The managed field a version publication, cursor, or fence access
    /// is over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<FieldPath>,

    /// The cursor rule of a cursor access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<CursorAdvanceRule>,
}

/// One lock declaration of a template, indexed apart from accesses: a
/// lock observes and mutates nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockAccess {
    pub transaction: TransactionRef,
    pub step: usize,
    pub object: Id,
    pub selector: ObjectSelector,
    pub mode: LockMode,
}

/// A lock cited as commit-order evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockRef {
    pub transaction: TransactionRef,
    pub step: usize,
    pub mode: LockMode,
}

/// The mechanism of one atomic conditional mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionalMutationKind {
    CompareAndSet,
    Transition,
    AdvanceCursor,
    Fence,
    UpsertIdentityArbitration,
}

impl ConditionalMutationKind {
    /// Whether every success of the mechanism mutates its instance, and
    /// so takes the instance's write protection and holds it to commit.
    /// A compare-and-set, a transition, a cursor advance (whose rules
    /// admit only a different position), and an upsert always do. A
    /// fence does not: on an equal token it leaves the instance as it
    /// was, so neither its fencing condition nor a comparison it carries
    /// keeps an observation from going stale — which is why a fence is
    /// never commit-order evidence on its own (§51).
    pub fn holds_protection(self) -> bool {
        !matches!(self, Self::Fence)
    }
}

impl fmt::Display for ConditionalMutationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::CompareAndSet => "compare-and-set",
            Self::Transition => "transition",
            Self::AdvanceCursor => "cursor advance",
            Self::Fence => "fence",
            Self::UpsertIdentityArbitration => "upsert",
        })
    }
}

/// The earlier read of the same instance whose same field a comparison
/// names: what makes it an observed-state comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedValue {
    pub read: Id,
    pub step: usize,
}

/// One comparison of a guarded mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComparisonFact {
    pub field: FieldPath,
    pub expected: SelectorValue,

    /// Set when `expected` is exactly the same field of an earlier read
    /// of the same instance in the same transaction — the only shape
    /// the prover credits as observed-state evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<ObservedValue>,
}

/// One atomic conditional mutation of a template: a compare-and-set, a
/// transition, a cursor advance, a fence, or an upsert's identity
/// arbitration, normalized (§15). Its condition and its mutation are
/// one storage operation, and once it succeeds the transaction holds
/// the instance's write protection to commit — which is why it is
/// commit-order evidence, and why a stale observation it compares
/// cannot participate in a successful commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConditionalMutation {
    pub transaction: TransactionRef,
    pub step: usize,
    pub target: ObjectSelector,
    pub mechanism: ConditionalMutationKind,
    pub comparisons: Vec<ComparisonFact>,
    pub writes: AccessFields,
    pub publishes_version: Option<FieldPath>,

    /// Whether the target pins the object's whole identity. Every
    /// credit the mechanism earns rests on it: a guard over a set or a
    /// range cannot see a concurrent insert of a new matching instance.
    pub identified: bool,
}

/// An artifact a transaction commits beside its state mutations.
/// Indexed for evidence; never a conflict access.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommitArtifact {
    /// An outbox admission — an ordinary `write_outbox` step, or a
    /// transition-scoped admission conditioned on the named transition
    /// applying.
    OutboxWrite {
        effect: Id,
        outbox: Id,
        schema: Id,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transition: Option<Id>,
    },

    EffectIntent {
        bind: Id,
    },

    TransactionOutput {
        bind: Id,
    },
}

/// Everything the analysis knows about one inline transaction.
#[derive(Debug, Clone)]
pub struct TransactionTemplate<'a> {
    pub reference: TransactionRef,
    pub transaction: &'a Transaction,
    pub accesses: Vec<TransactionAccess>,
    pub locks: Vec<LockAccess>,
    pub conditional_mutations: Vec<ConditionalMutation>,
    pub artifacts: Vec<CommitArtifact>,
}

/// The model-wide index the provers read.
#[derive(Debug)]
pub struct ConflictIndex<'a> {
    pub model: &'a Model,
    pub templates: Vec<TransactionTemplate<'a>>,

    /// Every data object by id, for identity and version lookups.
    objects: BTreeMap<&'a Id, &'a DataObject>,

    /// The state field each machine governs, with the subject object.
    states: BTreeMap<&'a Id, (&'a Id, &'a FieldPath)>,
}

impl<'a> ConflictIndex<'a> {
    pub fn build(model: &'a Model) -> Self {
        let mut objects = BTreeMap::new();

        for data_model in model.data_models.values() {
            for (object_id, object) in &data_model.objects {
                objects.insert(object_id, object);
            }
        }

        let mut states = BTreeMap::new();

        for (machine_id, machine) in &model.state_machines {
            let StateMachineSubject::Object { object, state } = &machine.subject;

            states.insert(machine_id, (object, state));
        }

        let mut index = Self {
            model,
            templates: Vec::new(),
            objects,
            states,
        };

        for (operation_id, operation) in &model.operations {
            for (location, transaction) in operation.program.transactions() {
                let reference = TransactionRef {
                    operation: operation_id.clone(),
                    transaction: transaction.id.clone(),
                    location,
                };

                let template = index.template_of(reference, transaction);

                index.templates.push(template);
            }
        }

        index
    }

    /// The declared version field of an object, if it is versioned.
    pub fn version_field(&self, object: &Id) -> Option<&'a FieldPath> {
        self.objects
            .get(object)
            .and_then(|object| object.version.as_ref())
            .map(|version| &version.field)
    }

    /// The object's declared identity fields, if the object exists.
    pub fn identity(&self, object: &Id) -> Option<&'a [FieldPath]> {
        self.objects
            .get(object)
            .map(|object| object.identity.as_slice())
    }

    /// Whether a selector pins every field of its object's (non-empty)
    /// identity by equality, so it selects at most one instance.
    pub fn identifies_instance(&self, selector: &ObjectSelector) -> bool {
        let Some(identity) = self.identity(&selector.object) else {
            return false;
        };

        let pinned = pins(&selector.predicate);

        !identity.is_empty()
            && identity
                .iter()
                .all(|field| pinned.iter().any(|(pinned, _)| *pinned == field))
    }

    /// A template that deletes some instance of the object, if any:
    /// after which an insertion may establish a version token a deleted
    /// instance already carried.
    pub fn deleter(&self, object: &Id) -> Option<&TransactionRef> {
        self.templates.iter().find_map(|template| {
            template
                .accesses
                .iter()
                .any(|access| access.mode == AccessMode::Delete && &access.object == object)
                .then_some(&template.reference)
        })
    }

    /// The position of the template declared by `operation` under the
    /// transaction id.
    pub fn position(&self, operation: &Id, transaction: &Id) -> Option<usize> {
        self.templates.iter().position(|template| {
            &template.reference.operation == operation
                && &template.reference.transaction == transaction
        })
    }

    fn template_of(
        &self,
        reference: TransactionRef,
        transaction: &'a Transaction,
    ) -> TransactionTemplate<'a> {
        let mut accesses = Vec::new();
        let mut locks = Vec::new();
        let mut conditional_mutations = Vec::new();
        let mut artifacts = Vec::new();

        let access = |step: usize,
                      selector: &ObjectSelector,
                      fields: AccessFields,
                      mode: AccessMode,
                      field: Option<FieldPath>,
                      rule: Option<CursorAdvanceRule>| TransactionAccess {
            transaction: reference.clone(),
            step,
            object: selector.object.clone(),
            selector: selector.clone(),
            fields,
            mode,
            field,
            rule,
        };

        let managed = |field: &FieldPath| AccessFields::Only(BTreeSet::from([field.clone()]));

        let declared = |fields: &BTreeSet<FieldPath>| {
            if fields.is_empty() {
                AccessFields::Unknown
            } else {
                AccessFields::Only(fields.clone())
            }
        };

        // Every committed mutation of a live versioned instance
        // publishes a newer token (§18): an effective write of the
        // version field, whether or not the step names it.
        let publication = |step: usize, selector: &ObjectSelector| {
            self.version_field(&selector.object).map(|version| {
                access(
                    step,
                    selector,
                    managed(version),
                    AccessMode::VersionPublish,
                    Some(version.clone()),
                    None,
                )
            })
        };

        // A guarded mutation's comparisons read their fields atomically
        // with the mutation.
        let comparison = |step: usize, selector: &ObjectSelector, compare: &[CompareCondition]| {
            (!compare.is_empty()).then(|| {
                access(
                    step,
                    selector,
                    AccessFields::Only(compare.iter().map(|c| c.field.clone()).collect()),
                    AccessMode::CompareRead,
                    None,
                    None,
                )
            })
        };

        let guarded = |step: usize,
                       target: &ObjectSelector,
                       mechanism: ConditionalMutationKind,
                       compare: &[CompareCondition],
                       writes: AccessFields| ConditionalMutation {
            transaction: reference.clone(),
            step,
            target: target.clone(),
            mechanism,
            comparisons: compare
                .iter()
                .map(|condition| ComparisonFact {
                    field: condition.field.clone(),
                    expected: condition.expected.clone(),
                    observed: observation(transaction, step, target, condition),
                })
                .collect(),
            writes,
            publishes_version: self.version_field(&target.object).cloned(),
            identified: self.identifies_instance(target),
        };

        for (step, inner) in transaction.steps.iter().enumerate() {
            match inner {
                TransactionStep::Read(read) => {
                    let fields = match &read.fields {
                        FieldSelection::All => AccessFields::All,
                        FieldSelection::Only(fields) => AccessFields::Only(fields.clone()),
                    };

                    accesses.push(access(
                        step,
                        &read.target,
                        fields,
                        AccessMode::Read,
                        None,
                        None,
                    ));
                }

                TransactionStep::Update(update) => {
                    accesses.push(access(
                        step,
                        &update.target,
                        declared(&update.fields),
                        AccessMode::Update,
                        None,
                        None,
                    ));

                    accesses.extend(publication(step, &update.target));
                }

                TransactionStep::CompareAndSet(cas) => {
                    accesses.extend(comparison(step, &cas.target, &cas.compare));

                    accesses.push(access(
                        step,
                        &cas.target,
                        declared(&cas.fields),
                        AccessMode::CompareWrite,
                        None,
                        None,
                    ));

                    accesses.extend(publication(step, &cas.target));

                    conditional_mutations.push(guarded(
                        step,
                        &cas.target,
                        ConditionalMutationKind::CompareAndSet,
                        &cas.compare,
                        declared(&cas.fields),
                    ));
                }

                TransactionStep::Insert(insert) => {
                    // An insert creates a complete logical instance
                    // whose identity is fixed by its derivation, which
                    // the analysis cannot read: it may touch any
                    // instance of the object.
                    let selector = ObjectSelector {
                        object: insert.object.clone(),
                        predicate: SelectorPredicate::All,
                    };

                    accesses.push(access(
                        step,
                        &selector,
                        AccessFields::All,
                        AccessMode::Insert,
                        None,
                        None,
                    ));
                }

                TransactionStep::Upsert(upsert) => {
                    // Either branch may run, and the insert branch
                    // writes the whole instance — the version token
                    // with it.
                    accesses.push(access(
                        step,
                        &upsert.target,
                        AccessFields::All,
                        AccessMode::UpsertReadWrite,
                        None,
                        None,
                    ));

                    conditional_mutations.push(guarded(
                        step,
                        &upsert.target,
                        ConditionalMutationKind::UpsertIdentityArbitration,
                        &[],
                        AccessFields::All,
                    ));
                }

                TransactionStep::Delete(delete) => {
                    accesses.push(access(
                        step,
                        &delete.target,
                        AccessFields::All,
                        AccessMode::Delete,
                        None,
                        None,
                    ));
                }

                TransactionStep::Lock(lock) => locks.push(LockAccess {
                    transaction: reference.clone(),
                    step,
                    object: lock.target.object.clone(),
                    selector: lock.target.clone(),
                    mode: lock.mode,
                }),

                TransactionStep::Transition(transition) => {
                    // The transition reads and writes the machine's
                    // state field of the subject; an unresolvable
                    // machine is conservatively the whole instance.
                    let fields = match self.states.get(&transition.machine) {
                        Some((_, state)) => managed(state),
                        None => AccessFields::All,
                    };

                    accesses.push(access(
                        step,
                        &transition.subject,
                        fields.clone(),
                        AccessMode::TransitionRead,
                        None,
                        None,
                    ));

                    accesses.extend(comparison(step, &transition.subject, &transition.compare));

                    accesses.push(access(
                        step,
                        &transition.subject,
                        fields.clone(),
                        AccessMode::TransitionWrite,
                        None,
                        None,
                    ));

                    accesses.extend(publication(step, &transition.subject));

                    conditional_mutations.push(guarded(
                        step,
                        &transition.subject,
                        ConditionalMutationKind::Transition,
                        &transition.compare,
                        fields,
                    ));

                    for effect_id in transition.effects.keys() {
                        let write = self
                            .model
                            .state_machines
                            .get(&transition.machine)
                            .and_then(|machine| machine.transitions.get(&transition.transition))
                            .and_then(|declared| declared.effects.get(effect_id))
                            .map(|declared| declared.outbox_write());

                        if let Some(write) = write {
                            artifacts.push(CommitArtifact::OutboxWrite {
                                effect: effect_id.clone(),
                                outbox: write.outbox.clone(),
                                schema: write.schema.clone(),
                                transition: Some(transition.transition.clone()),
                            });
                        }
                    }
                }

                TransactionStep::AdvanceCursor(advance) => {
                    accesses.push(access(
                        step,
                        &advance.target,
                        managed(&advance.field),
                        AccessMode::CursorReadWrite,
                        Some(advance.field.clone()),
                        Some(advance.rule),
                    ));

                    accesses.extend(comparison(step, &advance.target, &advance.compare));
                    accesses.extend(publication(step, &advance.target));

                    conditional_mutations.push(guarded(
                        step,
                        &advance.target,
                        ConditionalMutationKind::AdvanceCursor,
                        &advance.compare,
                        managed(&advance.field),
                    ));
                }

                TransactionStep::Fence(fence) => {
                    accesses.push(access(
                        step,
                        &fence.target,
                        managed(&fence.field),
                        AccessMode::FenceReadWrite,
                        Some(fence.field.clone()),
                        None,
                    ));

                    accesses.extend(comparison(step, &fence.target, &fence.compare));
                    accesses.extend(publication(step, &fence.target));

                    conditional_mutations.push(guarded(
                        step,
                        &fence.target,
                        ConditionalMutationKind::Fence,
                        &fence.compare,
                        managed(&fence.field),
                    ));
                }

                TransactionStep::EstablishEffectIntent(establish) => {
                    artifacts.push(CommitArtifact::EffectIntent {
                        bind: establish.bind.clone(),
                    });
                }

                TransactionStep::EstablishTransactionOutput(establish) => {
                    artifacts.push(CommitArtifact::TransactionOutput {
                        bind: establish.bind.clone(),
                    });
                }

                TransactionStep::WriteOutbox(write) => {
                    artifacts.push(CommitArtifact::OutboxWrite {
                        effect: write.effect_id.clone(),
                        outbox: write.effect.outbox.clone(),
                        schema: write.effect.schema.clone(),
                        transition: None,
                    });
                }
            }
        }

        // Transition applications bind intents too.
        for inner in &transaction.steps {
            if let TransactionStep::Transition(transition) = inner {
                for intent in transition.effect_intents.values() {
                    artifacts.push(CommitArtifact::EffectIntent {
                        bind: intent.bind.clone(),
                    });
                }
            }
        }

        TransactionTemplate {
            reference,
            transaction,
            accesses,
            locks,
            conditional_mutations,
            artifacts,
        }
    }

    // -----------------------------------------------------------------
    // Overlap
    // -----------------------------------------------------------------

    /// Whether two accesses may select overlapping instances (§39).
    pub fn selector_overlap(
        &self,
        a: &TransactionAccess,
        b: &TransactionAccess,
    ) -> SelectorOverlap {
        if a.object != b.object {
            return SelectorOverlap::Disjoint;
        }

        let pins_a = pins(&a.selector.predicate);
        let pins_b = pins(&b.selector.predicate);

        // Equality predicates proving incompatible literals on one
        // field prove disjoint domains.
        for (field_a, value_a) in &pins_a {
            for (field_b, value_b) in &pins_b {
                if field_a == field_b
                    && let (SelectorValue::Literal(literal_a), SelectorValue::Literal(literal_b)) =
                        (value_a, value_b)
                    && literal_a != literal_b
                {
                    return SelectorOverlap::Disjoint;
                }
            }
        }

        if matches!(a.selector.predicate, SelectorPredicate::All)
            && matches!(b.selector.predicate, SelectorPredicate::All)
        {
            return SelectorOverlap::Overlapping;
        }

        // Complete identities pinned by equal literals select one
        // instance.
        if let Some(identity) = self.identity(&a.object)
            && !identity.is_empty()
            && let (Some(literals_a), Some(literals_b)) = (
                identity_literals(identity, &pins_a),
                identity_literals(identity, &pins_b),
            )
            && literals_a == literals_b
        {
            return SelectorOverlap::Overlapping;
        }

        // Two executions evaluate their references independently, so
        // equal references prove nothing across them.
        SelectorOverlap::Unknown
    }

    /// The conflict between two accesses of (possibly the same)
    /// template, if any: at least one mutates, and neither the
    /// selected domains nor the fields are provably disjoint.
    pub fn conflict(&self, a: &TransactionAccess, b: &TransactionAccess) -> Option<Overlap> {
        if !(a.mode.writes() || b.mode.writes()) {
            return None;
        }

        let selector = self.selector_overlap(a, b);

        if selector == SelectorOverlap::Disjoint {
            return None;
        }

        let fields = field_overlap(&a.fields, &b.fields);

        if fields == FieldOverlap::Disjoint {
            return None;
        }

        Some(Overlap { selector, fields })
    }

    /// Whether two templates may conflict at all.
    fn templates_conflict(&self, s: usize, t: usize) -> bool {
        self.templates[s].accesses.iter().any(|a| {
            self.templates[t]
                .accesses
                .iter()
                .any(|b| self.conflict(a, b).is_some())
        })
    }

    /// The conflict closure of a template (§41): the connected
    /// component of the undirected potential-conflict graph containing
    /// it, in template order.
    pub fn closure(&self, root: usize) -> Vec<usize> {
        let mut members = BTreeSet::from([root]);
        let mut frontier = vec![root];

        while let Some(current) = frontier.pop() {
            for candidate in 0..self.templates.len() {
                if !members.contains(&candidate) && self.templates_conflict(current, candidate) {
                    members.insert(candidate);
                    frontier.push(candidate);
                }
            }
        }

        members.into_iter().collect()
    }

    // -----------------------------------------------------------------
    // Dependencies
    // -----------------------------------------------------------------

    /// Every potential serialization dependency among the templates of
    /// a closure (§43), each classified by its commit-order evidence.
    pub fn dependencies(&self, closure: &[usize]) -> Vec<DependencyEvidence> {
        let mut dependencies = Vec::new();

        for &s in closure {
            for &t in closure {
                for a in &self.templates[s].accesses {
                    for b in &self.templates[t].accesses {
                        let Some(overlap) = self.conflict(a, b) else {
                            continue;
                        };

                        if a.mode.writes() && b.mode.reads() {
                            dependencies.push(self.dependency(
                                DependencyKind::WriteRead,
                                s,
                                a,
                                t,
                                b,
                                &overlap,
                            ));
                        }

                        if a.mode.reads() && b.mode.writes() {
                            dependencies.push(self.dependency(
                                DependencyKind::ReadWriteAntiDependency,
                                s,
                                a,
                                t,
                                b,
                                &overlap,
                            ));
                        }

                        if a.mode.writes() && b.mode.writes() {
                            dependencies.push(self.dependency(
                                DependencyKind::WriteWrite,
                                s,
                                a,
                                t,
                                b,
                                &overlap,
                            ));
                        }
                    }
                }
            }
        }

        dependencies
    }

    fn dependency(
        &self,
        kind: DependencyKind,
        s: usize,
        a: &TransactionAccess,
        t: usize,
        b: &TransactionAccess,
        overlap: &Overlap,
    ) -> DependencyEvidence {
        let source = &self.templates[s];
        let target = &self.templates[t];

        let mut gaps = Vec::new();

        let fence = (a.mode == AccessMode::FenceReadWrite
            && b.mode == AccessMode::FenceReadWrite
            && a.field == b.field)
            .then(|| ManagedFieldRef {
                object: a.object.clone(),
                field: a.field.clone().unwrap_or_default(),
            });

        let evidence = if a.mode == AccessMode::CursorReadWrite
            && b.mode == AccessMode::CursorReadWrite
            && a.field == b.field
            && a.rule.is_some()
            && a.rule == b.rule
        {
            CommitOrderEvidence::OrderedCursor {
                object: a.object.clone(),
                field: a.field.clone().unwrap_or_default(),
                rule: a.rule.expect("checked above"),
            }
        } else {
            match kind {
                DependencyKind::WriteRead => {
                    if target.transaction.isolation != TransactionIsolation::Unspecified {
                        CommitOrderEvidence::IntrinsicCommittedRead {
                            isolation: target.transaction.isolation,
                        }
                    } else if let Some((guard, coverage)) = self.protection(target, b)
                        && self.holds_write(source, a)
                    {
                        // The reader observes at or after acquiring the
                        // instance's write protection, which the
                        // writer's own held write withholds until it
                        // terminates: what it reads has committed.
                        guard_evidence(target, guard, coverage, BTreeSet::new())
                    } else {
                        gaps.push(DependencyGap::IsolationUnspecified {
                            transaction: target.reference.clone(),
                        });

                        CommitOrderEvidence::None
                    }
                }

                DependencyKind::WriteWrite => {
                    let source_isolation = source.transaction.isolation;
                    let target_isolation = target.transaction.isolation;

                    if source_isolation != TransactionIsolation::Unspecified
                        && target_isolation != TransactionIsolation::Unspecified
                    {
                        CommitOrderEvidence::AtomicWriteOrder {
                            source_isolation,
                            target_isolation,
                        }
                    } else if let Some((guard, coverage)) = self.protection(source, a) {
                        // The earlier writer holds the instance's write
                        // protection from its guard to commit, so no
                        // later conflicting write commits before it.
                        guard_evidence(source, guard, coverage, BTreeSet::new())
                    } else if let Some((guard, coverage)) = self.protection(target, b)
                        && source_isolation != TransactionIsolation::Unspecified
                    {
                        // The later writer acquires the instance's
                        // write protection only once the earlier one,
                        // which holds its write under declared
                        // isolation, has terminated.
                        guard_evidence(target, guard, coverage, BTreeSet::new())
                    } else {
                        for (template, isolation) in
                            [(source, source_isolation), (target, target_isolation)]
                        {
                            if isolation == TransactionIsolation::Unspecified {
                                gaps.push(DependencyGap::IsolationUnspecified {
                                    transaction: template.reference.clone(),
                                });
                            }
                        }

                        CommitOrderEvidence::None
                    }
                }

                DependencyKind::ReadWriteAntiDependency => {
                    let reader_lock = self.covering_lock(source, a, false);
                    let writer_lock = self.covering_lock(target, b, true);

                    match (reader_lock, writer_lock) {
                        (Ok(reader_lock), Ok(writer_lock)) => CommitOrderEvidence::StrictLock {
                            reader_lock,
                            writer_lock,
                        },

                        (reader_lock, writer_lock) => match self.observation_guard(source, a, b) {
                            Ok(evidence) => evidence,

                            Err(_) if self.serializes_at_its_read(source) => {
                                CommitOrderEvidence::ReadOnlyObservation {
                                    isolation: source.transaction.isolation,
                                }
                            }

                            Err(guard_gap) => {
                                // A reader lock is held from before the
                                // observation to commit; a writer whose
                                // mutation is guarded must acquire the
                                // instance's write protection, which
                                // that lock withholds.
                                if let Ok(lock) = &reader_lock
                                    && let Some((guard, _)) = self.protection(target, b)
                                    && self.identifies_instance(&a.selector)
                                {
                                    guard_evidence(
                                        target,
                                        guard,
                                        GuardCoverage::LockedReader {
                                            reader_lock: lock.clone(),
                                        },
                                        BTreeSet::new(),
                                    )
                                } else {
                                    if let Err(gap) = reader_lock {
                                        gaps.push(gap);
                                    }

                                    if let Err(gap) = writer_lock {
                                        gaps.push(gap);
                                    }

                                    gaps.push(guard_gap);

                                    CommitOrderEvidence::None
                                }
                            }
                        },
                    }
                }
            }
        };

        if evidence == CommitOrderEvidence::None {
            if overlap.selector == SelectorOverlap::Unknown {
                gaps.push(DependencyGap::TransactionConflictUnknownSelectorOverlap {
                    object: a.object.clone(),
                });
            }

            if overlap.fields == FieldOverlap::Unknown {
                gaps.push(DependencyGap::TransactionConflictUnknownFieldOverlap {
                    object: a.object.clone(),
                });
            }
        }

        DependencyEvidence {
            kind,
            source: source.reference.clone(),
            source_step: a.step,
            source_mode: a.mode,
            target: target.reference.clone(),
            target_step: b.step,
            target_mode: b.mode,
            object: a.object.clone(),
            selector_overlap: overlap.selector,
            field_overlap: overlap.fields,
            evidence,
            fence,
            gaps,
        }
    }

    /// The guarded mutation of the template whose write protection
    /// covers the access: one of the same identified instance at the
    /// access's own step — the access is part of its atomic statement
    /// — or at an earlier step, whose protection is held to commit.
    pub fn protection<'t>(
        &self,
        template: &'t TransactionTemplate<'_>,
        access: &TransactionAccess,
    ) -> Option<(&'t ConditionalMutation, GuardCoverage)> {
        let guards = || {
            template.conditional_mutations.iter().filter(|guard| {
                guard.identified
                    && guard.mechanism.holds_protection()
                    && guard.target == access.selector
            })
        };

        guards()
            .find(|guard| guard.step == access.step)
            .map(|guard| (guard, GuardCoverage::Atomic))
            .or_else(|| {
                guards()
                    .find(|guard| guard.step < access.step)
                    .map(|guard| (guard, GuardCoverage::HeldProtection))
            })
    }

    /// Whether the template is a read-only observation that serializes
    /// at the instant it reads: it mutates nothing, reads only committed
    /// state under declared isolation, and observes at one instant — one
    /// read of one identified instance, or any reads under snapshot
    /// isolation, which see one snapshot. Serializable isolation is not
    /// enough for several reads: it may hold read locks rather than read
    /// a snapshot, and a writer of unspecified isolation need not
    /// respect them.
    ///
    /// Such a transaction's position in any serial order can be taken
    /// as that instant: every write it observed committed before it
    /// (its incoming dependencies are all write-read, and committed
    /// reads constrain them), and every write it missed commits after
    /// it. So its outgoing anti-dependencies need no mechanism of their
    /// own: a cycle through it whose other dependencies are all
    /// commit-ordered would order a writer's commit both before and
    /// after the observation. Two reads at different instants are not
    /// one observation — read skew passes between them — so under read
    /// committed only a single read qualifies.
    pub fn serializes_at_its_read(&self, template: &TransactionTemplate<'_>) -> bool {
        if template.accesses.iter().any(|access| access.mode.writes()) {
            return false;
        }

        match template.transaction.isolation {
            TransactionIsolation::Unspecified => false,

            TransactionIsolation::Snapshot => true,

            TransactionIsolation::ReadCommitted | TransactionIsolation::Serializable => {
                let mut reads = template
                    .accesses
                    .iter()
                    .filter(|access| access.mode.reads());

                let Some(first) = reads.next() else {
                    return true;
                };

                self.identifies_instance(&first.selector)
                    && reads.all(|access| {
                        access.step == first.step && access.selector == first.selector
                    })
            }
        }
    }

    /// Whether the template holds its write until it terminates: under
    /// declared isolation, or under a guarded mutation's protection.
    fn holds_write(&self, template: &TransactionTemplate<'_>, access: &TransactionAccess) -> bool {
        template.transaction.isolation != TransactionIsolation::Unspecified
            || self.protection(template, access).is_some()
    }

    /// Why the reader's observation `a` cannot go stale under the
    /// writer's access `b` and still participate in a successful
    /// commit (§22–§25), or the gap that leaves it unprotected.
    ///
    /// The observation is covered when it is part of, or follows, a
    /// guarded mutation of the same identified instance; or when a
    /// later guarded mutation of that instance compares what the read
    /// observed. The version token covers every observation of the
    /// instance from the read that observed it up to the guard: a live
    /// instance's token only ever grows, so any committed mutation in
    /// that window moves it. A direct comparison of fields covers only
    /// the read whose values it names — a field can change and change
    /// back, so an intervening observation may have seen a value the
    /// comparison no longer holds — and only the fields the conflict
    /// touches.
    #[allow(clippy::result_large_err)]
    pub fn observation_guard(
        &self,
        reader: &TransactionTemplate<'_>,
        a: &TransactionAccess,
        b: &TransactionAccess,
    ) -> Result<CommitOrderEvidence, DependencyGap> {
        if let Some((guard, coverage)) = self.protection(reader, a) {
            return Ok(guard_evidence(reader, guard, coverage, BTreeSet::new()));
        }

        if !self.identifies_instance(&a.selector) {
            return Err(DependencyGap::ObservedStateNotIdentified {
                transaction: reader.reference.clone(),
                object: a.object.clone(),
                step: a.step,
            });
        }

        let version = self.version_field(&a.object);
        let mut shortfall = None;

        for guard in reader.conditional_mutations.iter().filter(|guard| {
            guard.identified
                && guard.mechanism.holds_protection()
                && guard.step > a.step
                && guard.target == a.selector
        }) {
            // The version route: a comparison of the version some read at
            // or before the observation saw.
            let version_observed = version.and_then(|version| {
                guard.comparisons.iter().find_map(|comparison| {
                    comparison
                        .observed
                        .as_ref()
                        .filter(|observed| &comparison.field == version && observed.step <= a.step)
                        .map(|observed| (version, observed))
                })
            });

            if let Some((version, observed)) = version_observed {
                // An insertion after a deletion establishes its token
                // afresh and may repeat the one observed, so where the
                // object is ever deleted the token covers no insertion.
                let repeats = matches!(b.mode, AccessMode::Insert | AccessMode::UpsertReadWrite)
                    .then(|| self.deleter(&a.object))
                    .flatten();

                match repeats {
                    None => {
                        return Ok(guard_evidence(
                            reader,
                            guard,
                            GuardCoverage::ObservedVersion {
                                read: observed.read.clone(),
                                read_step: observed.step,
                                version_field: version.clone(),
                            },
                            BTreeSet::from([version.clone()]),
                        ));
                    }

                    Some(deleted_by) => {
                        shortfall.get_or_insert(DependencyGap::ObservedVersionMayRepeat {
                            transaction: reader.reference.clone(),
                            object: a.object.clone(),
                            step: a.step,
                            deleted_by: deleted_by.clone(),
                        });
                    }
                }
            }

            // The observed-state route: comparisons of the very read.
            let compared: BTreeSet<FieldPath> = guard
                .comparisons
                .iter()
                .filter(|comparison| {
                    comparison
                        .observed
                        .as_ref()
                        .is_some_and(|observed| observed.step == a.step)
                })
                .map(|comparison| comparison.field.clone())
                .collect();

            let Some(TransactionStep::Read(observing)) = reader.transaction.steps.get(a.step)
            else {
                continue;
            };

            if compared.is_empty() {
                continue;
            }

            match uncovered(&a.fields, &b.fields, &compared) {
                None => {
                    return Ok(guard_evidence(
                        reader,
                        guard,
                        GuardCoverage::ObservedState {
                            read: observing.bind.clone(),
                            read_step: a.step,
                        },
                        compared,
                    ));
                }

                Some(fields) => {
                    shortfall.get_or_insert(
                        DependencyGap::ObservedStateGuardDoesNotCoverConflict {
                            transaction: reader.reference.clone(),
                            object: a.object.clone(),
                            step: a.step,
                            guard_step: guard.step,
                            fields,
                        },
                    );
                }
            }
        }

        Err(
            shortfall.unwrap_or_else(|| DependencyGap::ObservedStateGuardMissing {
                transaction: reader.reference.clone(),
                object: a.object.clone(),
                step: a.step,
            }),
        )
    }

    /// The lock of the template that protects the access (§47, §48):
    /// on the same object, covering the selected domain, acquired at
    /// an earlier step, and exclusive when the access is a mutation.
    #[allow(clippy::result_large_err)]
    fn covering_lock(
        &self,
        template: &TransactionTemplate<'_>,
        access: &TransactionAccess,
        exclusive: bool,
    ) -> Result<LockRef, DependencyGap> {
        let side = if exclusive {
            DependencySide::Writer
        } else {
            DependencySide::Reader
        };

        let mut late = None;

        for lock in &template.locks {
            if lock.object != access.object
                || (exclusive && lock.mode != LockMode::Exclusive)
                || !covers(&lock.selector.predicate, &access.selector.predicate)
            {
                continue;
            }

            if lock.step < access.step {
                return Ok(LockRef {
                    transaction: template.reference.clone(),
                    step: lock.step,
                    mode: lock.mode,
                });
            }

            late.get_or_insert(lock.step);
        }

        Err(match late {
            Some(lock_step) => DependencyGap::LockAcquiredAfterProtectedAccess {
                transaction: template.reference.clone(),
                side,
                lock_step,
                access_step: access.step,
            },

            None => DependencyGap::LockCoverageMissing {
                transaction: template.reference.clone(),
                side,
                object: access.object.clone(),
                step: access.step,
            },
        })
    }

    // -----------------------------------------------------------------
    // Cycles
    // -----------------------------------------------------------------

    /// The cyclic strongly connected components of the dependency graph
    /// over a closure, each with its unconstrained member dependencies
    /// (§52). An SCC every one of whose dependencies is commit-order
    /// constrained is not returned: its apparent cycle would imply a
    /// cycle in strict commit order and cannot occur in a committed
    /// history.
    pub fn unconstrained_cycles(
        &self,
        closure: &[usize],
        dependencies: &[DependencyEvidence],
    ) -> Vec<UnconstrainedCycle> {
        let position = |reference: &TransactionRef| {
            closure
                .iter()
                .position(|&index| self.templates[index].reference == *reference)
        };

        let edges: Vec<(usize, usize)> = dependencies
            .iter()
            .filter_map(|dependency| {
                Some((position(&dependency.source)?, position(&dependency.target)?))
            })
            .collect();

        let mut cycles = Vec::new();

        for component in strongly_connected_components(closure.len(), &edges) {
            let cyclic = component.len() > 1
                || edges
                    .iter()
                    .any(|(from, to)| from == to && component.contains(from));

            if !cyclic {
                continue;
            }

            let members: Vec<TransactionRef> = component
                .iter()
                .map(|&local| self.templates[closure[local]].reference.clone())
                .collect();

            let unconstrained: Vec<DependencyEvidence> = dependencies
                .iter()
                .filter(|dependency| {
                    dependency.evidence == CommitOrderEvidence::None
                        && members.contains(&dependency.source)
                        && members.contains(&dependency.target)
                })
                .cloned()
                .collect();

            if !unconstrained.is_empty() {
                cycles.push(UnconstrainedCycle {
                    members,
                    unconstrained,
                });
            }
        }

        cycles
    }

    // -----------------------------------------------------------------
    // Value identity
    // -----------------------------------------------------------------

    /// Whether two references denote the same logical value in every
    /// evaluation by `operation`: the same source, and the same path
    /// or canonically equal paths in every schema the source admits.
    pub fn same_value(&self, operation: &Operation, a: &ValueRef, b: &ValueRef) -> bool {
        if a.source != b.source {
            return false;
        }

        if a.path == b.path {
            return true;
        }

        let Some(schemas) = self.value_schemas(operation, &a.source) else {
            return false;
        };

        !schemas.is_empty()
            && schemas.iter().all(|schema| {
                match (
                    canonical_value_path(self.model, schema, &a.path),
                    canonical_value_path(self.model, schema, &b.path),
                ) {
                    (Some(first), Some(second)) => first == second,
                    _ => false,
                }
            })
    }

    /// The payload schemas a value source resolves against within
    /// `operation`, where the analysis can tell.
    fn value_schemas(&self, operation: &Operation, source: &ValueSource) -> Option<Vec<Id>> {
        match source {
            ValueSource::Input(input) => match operation.inputs.get(input)? {
                Input::Request(request) => Some(vec![request.schema.clone()]),

                Input::Subscription(subscription) => Some(match &subscription.messages {
                    MessageSelector::Only(schemas) => schemas.iter().cloned().collect(),
                    MessageSelector::All => self
                        .model
                        .topics
                        .get(&subscription.topic)?
                        .messages
                        .iter()
                        .cloned()
                        .collect(),
                }),

                Input::Outbox(input) => Some(
                    self.model
                        .outbox(&input.outbox)?
                        .1
                        .messages
                        .iter()
                        .cloned()
                        .collect(),
                ),
            },

            ValueSource::TransactionOutput(output) => {
                for (_, transaction) in operation.program.transactions() {
                    for inner in &transaction.steps {
                        if let TransactionStep::EstablishTransactionOutput(establish) = inner
                            && &establish.bind == output
                        {
                            return Some(vec![establish.schema.clone()]);
                        }
                    }
                }

                None
            }

            ValueSource::StateMachineSubject(machine) => {
                let (object, _) = self.states.get(machine)?;

                Some(vec![self.objects.get(object)?.schema.clone()])
            }

            ValueSource::Effect(_)
            | ValueSource::TransactionRead(_)
            | ValueSource::EffectResultOk(_)
            | ValueSource::EffectResultErr(_) => None,
        }
    }

    /// Whether a requirement key identifies the domain a selector
    /// addresses (§55): every identity field of the object is pinned,
    /// by a literal or by a reference to the key itself, so equal keys
    /// select one instance and different keys never share one.
    pub fn key_identifies_domain(
        &self,
        operation: &Operation,
        selector: &ObjectSelector,
        key: &ValueRef,
    ) -> bool {
        let Some(identity) = self.identity(&selector.object) else {
            return false;
        };

        if identity.is_empty() {
            return false;
        }

        let pins = pins(&selector.predicate);

        let mut keyed = false;

        for field in identity {
            let Some((_, value)) = pins.iter().find(|(pinned, _)| *pinned == field) else {
                return false;
            };

            match value {
                SelectorValue::Literal(_) => {}

                SelectorValue::Value(reference) => {
                    if !self.same_value(operation, reference, key) {
                        return false;
                    }

                    keyed = true;
                }
            }
        }

        keyed
    }

    /// Every write to a managed field that is not the protocol's own:
    /// an update, compare-and-set, or upsert update naming the field,
    /// or a cursor advance of it under a different rule (§55, §56).
    pub fn uncontrolled_managed_writers(
        &self,
        object: &Id,
        field: &FieldPath,
        rule: Option<CursorAdvanceRule>,
    ) -> Vec<(TransactionRef, usize)> {
        let mut writers = Vec::new();

        for template in &self.templates {
            for access in &template.accesses {
                if &access.object != object {
                    continue;
                }

                let touches = |fields: &AccessFields| match fields {
                    AccessFields::Only(fields) => fields.iter().any(|written| {
                        written.0.starts_with(&field.0) || field.0.starts_with(&written.0)
                    }),
                    AccessFields::All | AccessFields::Unknown => true,
                };

                let uncontrolled = match access.mode {
                    AccessMode::Update | AccessMode::CompareWrite => touches(&access.fields),

                    // Only the update branch assigns an existing
                    // instance's fields; the insert branch initializes
                    // one, as an insert does.
                    AccessMode::UpsertReadWrite => match &template.transaction.steps[access.step] {
                        TransactionStep::Upsert(upsert) => {
                            touches(&AccessFields::Only(upsert.update_fields.clone()))
                        }
                        _ => true,
                    },

                    AccessMode::CursorReadWrite => {
                        access.field.as_ref() == Some(field) && access.rule != rule
                    }

                    AccessMode::FenceReadWrite => {
                        access.field.as_ref() == Some(field) && rule.is_some()
                    }

                    _ => false,
                };

                if uncontrolled {
                    writers.push((template.reference.clone(), access.step));
                }
            }
        }

        writers
    }
}

/// The earlier read a guarded mutation's comparison observes, when the
/// comparison is an observed-state one (§9): `expected` is exactly
/// `transaction_read:<bind>.<the compared field>`, and `<bind>` is a
/// read that precedes the comparison, selects the same instance, and
/// covers the field.
pub fn observation(
    transaction: &Transaction,
    step: usize,
    target: &ObjectSelector,
    condition: &CompareCondition,
) -> Option<ObservedValue> {
    let bind = condition.observed_read()?;

    transaction
        .steps
        .iter()
        .enumerate()
        .take(step)
        .find_map(|(position, inner)| match inner {
            TransactionStep::Read(read)
                if &read.bind == bind
                    && read.target == *target
                    && match &read.fields {
                        FieldSelection::All => true,
                        FieldSelection::Only(fields) => fields
                            .iter()
                            .any(|field| condition.field.0.starts_with(&field.0)),
                    } =>
            {
                Some(ObservedValue {
                    read: bind.clone(),
                    step: position,
                })
            }

            _ => None,
        })
}

/// The evidence a guarded mutation of `template` supplies.
fn guard_evidence(
    template: &TransactionTemplate<'_>,
    guard: &ConditionalMutation,
    coverage: GuardCoverage,
    compared: BTreeSet<FieldPath>,
) -> CommitOrderEvidence {
    CommitOrderEvidence::AtomicConditionalMutation {
        guarded_by: template.reference.clone(),
        step: guard.step,
        object: guard.target.object.clone(),
        mechanism: guard.mechanism,
        guard: coverage,
        compared_fields: if compared.is_empty() {
            guard
                .comparisons
                .iter()
                .map(|comparison| comparison.field.clone())
                .collect()
        } else {
            compared
        },
    }
}

/// The conflicting regions of a read footprint and a write footprint
/// that no compared field covers, or `None` when every one is covered.
/// A compared field covers itself and everything nested in it; where
/// either footprint is undeclared, or both are whole instances, the
/// conflict cannot be enumerated and nothing is covered.
fn uncovered(
    read: &AccessFields,
    written: &AccessFields,
    compared: &BTreeSet<FieldPath>,
) -> Option<AccessFields> {
    let covered = |region: &FieldPath| compared.iter().any(|field| region.0.starts_with(&field.0));

    let regions: BTreeSet<FieldPath> = match (read, written) {
        (AccessFields::Unknown, _) | (_, AccessFields::Unknown) => {
            return Some(AccessFields::Unknown);
        }

        (AccessFields::All, AccessFields::All) => return Some(AccessFields::All),

        (AccessFields::All, AccessFields::Only(fields))
        | (AccessFields::Only(fields), AccessFields::All) => fields.clone(),

        (AccessFields::Only(read), AccessFields::Only(written)) => read
            .iter()
            .flat_map(|r| {
                written.iter().filter_map(move |w| {
                    if r.0.starts_with(&w.0) {
                        Some(r.clone())
                    } else if w.0.starts_with(&r.0) {
                        Some(w.clone())
                    } else {
                        None
                    }
                })
            })
            .collect(),
    };

    let missing: BTreeSet<FieldPath> = regions
        .into_iter()
        .filter(|region| !covered(region))
        .collect();

    (!missing.is_empty()).then_some(AccessFields::Only(missing))
}

/// The `Eq` constraints of a predicate, flattened.
fn pins(predicate: &SelectorPredicate) -> Vec<(&FieldPath, &SelectorValue)> {
    match predicate {
        SelectorPredicate::All => Vec::new(),
        SelectorPredicate::Eq { field, value } => vec![(field, value)],
        SelectorPredicate::And { predicates } => predicates.iter().flat_map(pins).collect(),
    }
}

/// The literals pinning every identity field, when every one is pinned
/// by a literal.
fn identity_literals<'p>(
    identity: &[FieldPath],
    pins: &[(&'p FieldPath, &'p SelectorValue)],
) -> Option<Vec<&'p Literal>> {
    identity
        .iter()
        .map(|field| {
            pins.iter().find_map(|(pinned, value)| match value {
                SelectorValue::Literal(literal) if *pinned == field => Some(literal),
                _ => None,
            })
        })
        .collect()
}

/// Whether a lock predicate covers an access predicate: everything the
/// lock constrains, the access constrains identically, so the access
/// selects within the locked domain. An unconstrained lock covers
/// everything; an unconstrained access is covered only by one.
fn covers(lock: &SelectorPredicate, access: &SelectorPredicate) -> bool {
    let locked = pins(lock);
    let accessed = pins(access);

    locked
        .iter()
        .all(|(field, value)| accessed.iter().any(|(f, v)| f == field && v == value))
}

/// Whether two field footprints may overlap (§40).
pub fn field_overlap(a: &AccessFields, b: &AccessFields) -> FieldOverlap {
    match (a, b) {
        (AccessFields::Unknown, _) | (_, AccessFields::Unknown) => FieldOverlap::Unknown,

        (AccessFields::All, _) | (_, AccessFields::All) => FieldOverlap::Overlapping,

        (AccessFields::Only(first), AccessFields::Only(second)) => {
            let related = first.iter().any(|f| {
                second
                    .iter()
                    .any(|g| f.0.starts_with(&g.0) || g.0.starts_with(&f.0))
            });

            if related {
                FieldOverlap::Overlapping
            } else {
                FieldOverlap::Disjoint
            }
        }
    }
}

/// Tarjan's algorithm over `count` nodes: the strongly connected
/// components, in a deterministic order.
fn strongly_connected_components(count: usize, edges: &[(usize, usize)]) -> Vec<Vec<usize>> {
    struct State<'e> {
        edges: &'e [(usize, usize)],
        index: usize,
        indices: Vec<Option<usize>>,
        lowlink: Vec<usize>,
        on_stack: Vec<bool>,
        stack: Vec<usize>,
        components: Vec<Vec<usize>>,
    }

    fn visit(state: &mut State<'_>, node: usize) {
        state.indices[node] = Some(state.index);
        state.lowlink[node] = state.index;
        state.index += 1;
        state.stack.push(node);
        state.on_stack[node] = true;

        let successors: Vec<usize> = state
            .edges
            .iter()
            .filter(|(from, _)| *from == node)
            .map(|(_, to)| *to)
            .collect();

        for next in successors {
            match state.indices[next] {
                None => {
                    visit(state, next);
                    state.lowlink[node] = state.lowlink[node].min(state.lowlink[next]);
                }

                Some(index) if state.on_stack[next] => {
                    state.lowlink[node] = state.lowlink[node].min(index);
                }

                Some(_) => {}
            }
        }

        if state.lowlink[node] == state.indices[node].expect("visited") {
            let mut component = Vec::new();

            while let Some(member) = state.stack.pop() {
                state.on_stack[member] = false;
                component.push(member);

                if member == node {
                    break;
                }
            }

            component.sort_unstable();
            state.components.push(component);
        }
    }

    let mut state = State {
        edges,
        index: 0,
        indices: vec![None; count],
        lowlink: vec![0; count],
        on_stack: vec![false; count],
        stack: Vec::new(),
        components: Vec::new(),
    };

    for node in 0..count {
        if state.indices[node].is_none() {
            visit(&mut state, node);
        }
    }

    state.components.sort();

    state.components
}

/// Whether two selected domains may overlap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectorOverlap {
    Disjoint,

    /// Proven to select common instances.
    Overlapping,

    /// Not excluded — treated as overlapping.
    Unknown,
}

/// Whether two field footprints may overlap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldOverlap {
    Disjoint,
    Overlapping,

    /// A footprint is undeclared — treated as overlapping.
    Unknown,
}

/// The overlap facts behind one conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Overlap {
    pub selector: SelectorOverlap,
    pub fields: FieldOverlap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    /// The target reads what the source wrote.
    WriteRead,

    /// The source read state the target then overwrote — the
    /// anti-dependency behind write skew.
    ReadWriteAntiDependency,

    /// Both write; the target's write is installed after the source's.
    WriteWrite,
}

impl fmt::Display for DependencyKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::WriteRead => "write-read",
            Self::ReadWriteAntiDependency => "read-write anti-dependency",
            Self::WriteWrite => "write-write",
        })
    }
}

/// Which side of a dependency a gap concerns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencySide {
    Reader,
    Writer,
}

impl fmt::Display for DependencySide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Reader => "reader",
            Self::Writer => "writer",
        })
    }
}

/// Why the model guarantees that, whenever both transactions commit
/// and the dependency occurs, the source commits before the target
/// (§45).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommitOrderEvidence {
    /// The reader observes only committed writes, so the writer had
    /// committed before the observation (§46).
    IntrinsicCommittedRead { isolation: TransactionIsolation },

    /// Conflicting writes under declared isolation are installed in
    /// commit order (§46).
    AtomicWriteOrder {
        source_isolation: TransactionIsolation,
        target_isolation: TransactionIsolation,
    },

    /// Both templates lock the domain before their conflicting
    /// accesses and hold the locks to termination, so the reader's
    /// commit precedes the writer's exclusive acquisition (§47).
    StrictLock {
        reader_lock: LockRef,
        writer_lock: LockRef,
    },

    /// An atomic conditional mutation of the named transaction — a
    /// compare-and-set, transition, cursor advance, fence, or upsert —
    /// constrains the order: its condition and mutation are one storage
    /// operation whose write protection is held to commit, and the
    /// `guard` says why it covers this dependency (§21–§26).
    AtomicConditionalMutation {
        guarded_by: TransactionRef,

        /// The guarded mutation's step.
        step: usize,

        object: Id,
        mechanism: ConditionalMutationKind,
        guard: GuardCoverage,

        #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
        compared_fields: BTreeSet<FieldPath>,
    },

    /// The reader only reads, and observes committed state at one
    /// instant, which serves as its serialization point: every write it
    /// observed committed before it, every write it missed commits after
    /// it, so no cycle of otherwise commit-ordered dependencies can pass
    /// through it.
    ReadOnlyObservation { isolation: TransactionIsolation },

    /// Both accesses advance one cursor domain under one rule, so the
    /// accepted positions order the commits (§50).
    OrderedCursor {
        object: Id,
        field: FieldPath,
        rule: CursorAdvanceRule,
    },

    /// No declared fact constrains the commit order.
    None,
}

/// Why an atomic conditional mutation covers a dependency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GuardCoverage {
    /// The access is part of the guarded mutation's own atomic
    /// statement: its condition or its mutation.
    Atomic,

    /// The access follows the guarded mutation of the same instance,
    /// under the write protection it holds to commit.
    HeldProtection,

    /// The guarded mutation compares, against their observed values,
    /// every field of the instance the earlier read observed that the
    /// conflict touches.
    ObservedState { read: Id, read_step: usize },

    /// The guarded mutation compares the object's version token against
    /// the one the earlier read observed — at or before the access it
    /// covers; every committed mutation of the live instance publishes
    /// a newer token, and a deletion leaves none to match.
    ObservedVersion {
        read: Id,
        read_step: usize,
        version_field: FieldPath,
    },

    /// The reader holds a lock on the instance from before its
    /// observation to commit, and the writer's guarded mutation must
    /// acquire the instance's write protection, which that lock
    /// withholds until the reader terminates.
    LockedReader { reader_lock: LockRef },
}

/// A missing premise of commit-order evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DependencyGap {
    /// The side holds no lock covering the accessed domain — none at
    /// all, or none of the required mode.
    LockCoverageMissing {
        transaction: TransactionRef,
        side: DependencySide,
        object: Id,
        step: usize,
    },

    /// A covering lock exists but is acquired after the access it
    /// would have to protect; a lock protects no earlier observation
    /// (§48).
    LockAcquiredAfterProtectedAccess {
        transaction: TransactionRef,
        side: DependencySide,
        lock_step: usize,
        access_step: usize,
    },

    /// The reader observes one identified instance, and no later atomic
    /// mutation of it compares the observed state or version: nothing
    /// keeps a stale observation from participating in a commit.
    ObservedStateGuardMissing {
        transaction: TransactionRef,
        object: Id,
        step: usize,
    },

    /// A later guarded mutation compares part of what the read
    /// observed, but not every field the conflict touches.
    ObservedStateGuardDoesNotCoverConflict {
        transaction: TransactionRef,
        object: Id,
        step: usize,
        guard_step: usize,
        fields: AccessFields,
    },

    /// The reader observes a set or range rather than one identified
    /// instance, so no conditional mutation of one instance can stand
    /// for it — a concurrent insert of a new matching instance escapes
    /// every such guard (§25).
    ObservedStateNotIdentified {
        transaction: TransactionRef,
        object: Id,
        step: usize,
    },

    /// The reader's guard compares the observed version, but the
    /// conflict is an insertion and the object is deleted elsewhere: an
    /// instance inserted after a deletion establishes its token afresh,
    /// and may repeat the one observed.
    ObservedVersionMayRepeat {
        transaction: TransactionRef,
        object: Id,
        step: usize,
        deleted_by: TransactionRef,
    },

    /// The transaction declares no isolation, so nothing says it reads
    /// committed data or installs conflicting writes in commit order.
    IsolationUnspecified { transaction: TransactionRef },

    /// The dependency exists because the selected domains could not be
    /// proven disjoint, not because they were proven to overlap.
    TransactionConflictUnknownSelectorOverlap { object: Id },

    /// The dependency exists because a field footprint is undeclared.
    TransactionConflictUnknownFieldOverlap { object: Id },
}

/// One potential dependency with its evidence (§43, §53).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyEvidence {
    pub kind: DependencyKind,

    pub source: TransactionRef,
    pub source_step: usize,
    pub source_mode: AccessMode,

    pub target: TransactionRef,
    pub target_step: usize,
    pub target_mode: AccessMode,

    pub object: Id,
    pub selector_overlap: SelectorOverlap,
    pub field_overlap: FieldOverlap,

    pub evidence: CommitOrderEvidence,

    /// Both accesses fence one field. Recorded apart from the evidence:
    /// equal fencing tokens serialize nothing, so a fence constrains no
    /// dependency by itself (§51).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fence: Option<ManagedFieldRef>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<DependencyGap>,
}

impl DependencyEvidence {
    pub fn constrained(&self) -> bool {
        self.evidence != CommitOrderEvidence::None
    }
}

/// A cyclic strongly connected component with at least one
/// unconstrained dependency: a committed history Conseqa cannot exclude
/// from being non-serializable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnconstrainedCycle {
    pub members: Vec<TransactionRef>,
    pub unconstrained: Vec<DependencyEvidence>,
}

/// The label of a declared isolation level, as the DSL spells it.
pub fn isolation_label(isolation: TransactionIsolation) -> &'static str {
    match isolation {
        TransactionIsolation::Unspecified => "unspecified",
        TransactionIsolation::ReadCommitted => "read_committed",
        TransactionIsolation::Snapshot => "snapshot",
        TransactionIsolation::Serializable => "serializable",
    }
}
