//! Model-wide transaction conflict analysis: the shared machinery of
//! the transaction serializability and ordering provers (§35–§52 of
//! the transaction-consistency revision).
//!
//! A serializability obligation is a system-wide property over
//! potentially conflicting transaction executions, so no prover may
//! examine the declaring transaction alone. This module derives, once
//! per model:
//!
//! - **the access index** — every persistent-state access of every
//!   inline transaction template, with its object, selector domain,
//!   field footprint, and mode; lock declarations, version validations
//!   and bumps, and commit artifacts (outbox admissions, intent
//!   establishments, outputs) are indexed beside it and never counted
//!   as conflict accesses;
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
//! conflicting, an unspecified isolation no isolation, and incomplete
//! version, cursor, or fence coverage no credit at all. One inline
//! declaration is one template, and concurrent executions of the same
//! template are analyzed as two, so a template may conflict with
//! itself.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::spec::{
    CursorAdvanceRule, DataObject, FieldPath, FieldSelection, Id, Input, Literal, LockMode,
    MessageSelector, Model, ObjectSelector, Operation, SelectorPredicate, SelectorValue,
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
    Write,
    Insert,
    Delete,
    TransitionRead,
    TransitionWrite,
    VersionValidate,
    VersionBump,
    CursorReadWrite,
    FenceReadWrite,
}

impl AccessMode {
    /// Whether the access observes state.
    pub fn reads(self) -> bool {
        matches!(
            self,
            Self::Read
                | Self::TransitionRead
                | Self::VersionValidate
                | Self::CursorReadWrite
                | Self::FenceReadWrite
        )
    }

    /// Whether the access mutates state.
    pub fn writes(self) -> bool {
        matches!(
            self,
            Self::Write
                | Self::Insert
                | Self::Delete
                | Self::TransitionWrite
                | Self::VersionBump
                | Self::CursorReadWrite
                | Self::FenceReadWrite
        )
    }
}

impl fmt::Display for AccessMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Insert => "insert",
            Self::Delete => "delete",
            Self::TransitionRead => "transition read",
            Self::TransitionWrite => "transition write",
            Self::VersionValidate => "version validation",
            Self::VersionBump => "version bump",
            Self::CursorReadWrite => "cursor advance",
            Self::FenceReadWrite => "fence",
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

    /// The footprint is not declared — a write naming no fields — so
    /// its provenance is unknown and it is treated as potentially
    /// conflicting with everything.
    Unknown,
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

    /// The managed field a version, cursor, or fence access is over.
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

/// A `ValidateVersion` step of a template, with whether its expected
/// version is a preceding observation of the same instance's declared
/// version field — the only shape that makes it a commit guard the
/// prover may credit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionValidation {
    pub step: usize,
    pub selector: ObjectSelector,
    pub observed: bool,
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
    pub validations: Vec<VersionValidation>,

    /// `BumpVersion` steps: step index and selected instance.
    pub bumps: Vec<(usize, ObjectSelector)>,

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
        let mut validations = Vec::new();
        let mut bumps = Vec::new();
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

                TransactionStep::Write(write) => {
                    let fields = if write.fields.is_empty() {
                        AccessFields::Unknown
                    } else {
                        AccessFields::Only(write.fields.clone())
                    };

                    accesses.push(access(
                        step,
                        &write.target,
                        fields,
                        AccessMode::Write,
                        None,
                        None,
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

                    accesses.push(access(
                        step,
                        &transition.subject,
                        fields,
                        AccessMode::TransitionWrite,
                        None,
                        None,
                    ));

                    for (effect_id, effect) in &transition.effects {
                        let write = self
                            .model
                            .state_machines
                            .get(&transition.machine)
                            .and_then(|machine| machine.transitions.get(&transition.transition))
                            .and_then(|declared| declared.effects.get(effect_id))
                            .map(|declared| declared.outbox_write());

                        let _ = effect;

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

                TransactionStep::ValidateVersion(validate) => {
                    let field = self.version_field(&validate.target.object);

                    let fields = field.map(managed).unwrap_or(AccessFields::All);

                    accesses.push(access(
                        step,
                        &validate.target,
                        fields,
                        AccessMode::VersionValidate,
                        field.cloned(),
                        None,
                    ));

                    validations.push(VersionValidation {
                        step,
                        selector: validate.target.clone(),
                        observed: field.is_some_and(|field| {
                            observes_version(
                                transaction,
                                step,
                                &validate.target,
                                field,
                                &validate.expected,
                            )
                        }),
                    });
                }

                TransactionStep::BumpVersion(bump) => {
                    let field = self.version_field(&bump.target.object);

                    let fields = field.map(managed).unwrap_or(AccessFields::All);

                    accesses.push(access(
                        step,
                        &bump.target,
                        fields,
                        AccessMode::VersionBump,
                        field.cloned(),
                        None,
                    ));

                    bumps.push((step, bump.target.clone()));
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
            validations,
            bumps,
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
                    } else if self.validates(target, b) && self.bumps(source, a) {
                        self.version_evidence(target, &b.object)
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
                    } else if self.validates(target, b) && self.bumps(source, a) {
                        self.version_evidence(target, &b.object)
                    } else if self.validates(source, a) && self.bumps(target, b) {
                        self.version_evidence(source, &a.object)
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

                        (reader_lock, writer_lock) => {
                            if self.validates(source, a) && self.bumps(target, b) {
                                self.version_evidence(source, &a.object)
                            } else {
                                if let Err(gap) = reader_lock {
                                    gaps.push(gap);
                                }

                                if let Err(gap) = writer_lock {
                                    gaps.push(gap);
                                }

                                if !self.validates(source, a) {
                                    gaps.push(DependencyGap::VersionValidationMissing {
                                        transaction: source.reference.clone(),
                                        object: a.object.clone(),
                                    });
                                } else {
                                    gaps.push(DependencyGap::VersionBumpMissing {
                                        transaction: target.reference.clone(),
                                        object: b.object.clone(),
                                    });
                                }

                                CommitOrderEvidence::None
                            }
                        }
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

    fn version_evidence(
        &self,
        validated_by: &TransactionTemplate<'_>,
        object: &Id,
    ) -> CommitOrderEvidence {
        CommitOrderEvidence::VersionValidation {
            validated_by: validated_by.reference.clone(),
            object: object.clone(),
            field: self.version_field(object).cloned().unwrap_or_default(),
        }
    }

    /// Whether the template validates the selected instance's version
    /// against a preceding observation of it.
    fn validates(&self, template: &TransactionTemplate<'_>, access: &TransactionAccess) -> bool {
        template
            .validations
            .iter()
            .any(|validation| validation.observed && validation.selector == access.selector)
    }

    /// Whether the template's mutation of the selected instance cannot
    /// escape a validated observation of it: a `BumpVersion` of the
    /// same instance, the access being that bump, a delete of the
    /// instance (the validation finds no version to match), or an
    /// insert (the validated instance either already exists, so the
    /// insert fails uniqueness, or does not, so the validation had no
    /// version to observe).
    fn bumps(&self, template: &TransactionTemplate<'_>, access: &TransactionAccess) -> bool {
        matches!(
            access.mode,
            AccessMode::VersionBump | AccessMode::Delete | AccessMode::Insert
        ) || template
            .bumps
            .iter()
            .any(|(_, selector)| *selector == access.selector)
    }

    /// The lock of the template that protects the access (§47, §48):
    /// on the same object, covering the selected domain, acquired at
    /// an earlier step, and exclusive when the access is a mutation.
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
    /// an ordinary write naming the field, or a cursor advance of it
    /// under a different rule (§55, §56).
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

                let uncontrolled = match access.mode {
                    AccessMode::Write => match &access.fields {
                        AccessFields::Only(fields) => fields.iter().any(|written| {
                            written.0.starts_with(&field.0) || field.0.starts_with(&written.0)
                        }),
                        AccessFields::All | AccessFields::Unknown => true,
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

/// Whether a `ValidateVersion` step's expected version is a preceding
/// read of the same instance's version field: the read selects the
/// same instance, precedes the validation, covers the version field,
/// and the expected reference names exactly that field of that read.
pub fn observes_version(
    transaction: &Transaction,
    validate_step: usize,
    target: &ObjectSelector,
    version: &FieldPath,
    expected: &ValueRef,
) -> bool {
    let ValueSource::TransactionRead(bind) = &expected.source else {
        return false;
    };

    if &expected.path != version {
        return false;
    }

    transaction
        .steps
        .iter()
        .take(validate_step)
        .any(|step| match step {
            TransactionStep::Read(read) => {
                &read.bind == bind
                    && read.target == *target
                    && match &read.fields {
                        FieldSelection::All => true,
                        FieldSelection::Only(fields) => {
                            fields.iter().any(|field| version.0.starts_with(&field.0))
                        }
                    }
            }

            _ => false,
        })
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

    /// The source read a version the target then overwrote — the
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

    /// The named transaction validates the instance's version at
    /// commit against the version it observed, and the other side's
    /// mutation advances that version, so a stale observation rejects
    /// rather than commits (§49).
    VersionValidation {
        validated_by: TransactionRef,
        object: Id,
        field: FieldPath,
    },

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

    /// The reader does not validate the observed instance's version
    /// at commit.
    VersionValidationMissing {
        transaction: TransactionRef,
        object: Id,
    },

    /// The writer does not advance the version the reader validates.
    VersionBumpMissing {
        transaction: TransactionRef,
        object: Id,
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
