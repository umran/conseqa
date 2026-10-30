//! The basis of a verdict: the facts its proof consumed, stated
//! without positions (§9 and §10 of
//! `Conseqa_Spec_Change_Analysis_Revision_Specification.md`).
//!
//! An obligation's positional `id` is its address within one report.
//! Its [`ObligationKey`] is its identity across reports, so reordering
//! requirements renames no obligation (§9). A proven verdict's basis is
//! the set of facts it consumed, declared facts and model-wide query
//! results alike, each anchored to the declarations its truth was read
//! from. That is what lets a report or a diff answer "which proofs rest
//! on this declaration?" (§10.1). An unproven verdict has a
//! [`BlockedOn`] set instead: the facts its obstacles found present but
//! insufficient, the facts they found missing, and the declarations
//! they read (§10.7).
//!
//! The basis is a proof's read set, the counterpart of the design
//! engine's `TaskReadSet`: a proof depends on what is absent exactly as
//! it depends on what is present, which is why `no_deleters` and the
//! conflict closure are facts.
//!
//! Facts outlive the run that computed them. The obligation report, the
//! diff document, and a repository's conformance records all store
//! them, and a record written by one build may be read by another months
//! later. So every fact has one canonical durable form, counted by
//! [`FACT_FORMAT`] and guarded by golden files (§10.9). No analyzer type
//! appears in a fact: the analyzer may rename its structures freely,
//! while a fact's spelling changes only with a format bump.

mod declaration;
mod durable;
mod extract;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::spec::{
    CursorAdvanceRule, DeliverySemantics, ExternalIdentity, FieldPath, Id, Literal, LockMode,
    ObjectSelector, SelectorPredicate, SelectorValue, TransactionIsolation, ValueRef,
};

pub use declaration::{DeclarationKey, TextualKey, TextualKeyError};
pub use durable::{CanonicalError, FactVersionError, check_versions, read_canonical};
pub use extract::{Extractor, Grounds};

/// The fact vocabulary's format, counting every change to it: a kind
/// added, removed, or renamed, or a field added, removed, renamed, or
/// retyped (§10.9). A `dsl` bump forces a bump here, since facts are
/// stated in the DSL's vocabulary; a bump here never forces a `dsl`
/// bump.
///
/// Format 1 is the vocabulary of the spec change analysis revision,
/// with the `realization` kind added by the realization profiles
/// revision (Part VI §12) before it shipped.
pub const FACT_FORMAT: FactFormat = FactFormat(1);

/// The fact formats this build reads: the current one, and every
/// earlier one it extends without changing anything that one said. A
/// record declaring one is read as the current format, mirroring the
/// DSL's `DSL_READS`.
pub const FACT_READS: [FactFormat; 1] = [FactFormat(1)];

/// A declared fact vocabulary format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FactFormat(pub u64);

impl fmt::Display for FactFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

// ---------------------------------------------------------------------
// Obligation identity
// ---------------------------------------------------------------------

/// The identity of an obligation across reports (§9.2).
///
/// It is what the obligation is about: a property, of a subject, for
/// the executions sharing a requirement key. Moving a requirement
/// within its list, adding one beside it, or renumbering a
/// transaction's steps leaves it unchanged. Changing the key itself is
/// a different obligation, and changing a requirement's terms (an
/// ordering position, a completion) restates the same one (§9.3, §9.4).
///
/// The key holds canonical value references, not a hash, so it stays
/// meaningful outside the run that computed it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObligationKey {
    /// The report subject, without the requirement index.
    pub subject: ObligationSubject,

    pub property: ObligationProperty,

    /// The value references identifying the requirement, in declared
    /// order: the key of a `SerializableBy` or `OrderedBy`, or the
    /// components of an idempotency or recoverability key.
    pub requirement_key: Vec<ValueRef>,

    /// Which of the requirements sharing subject, property, and
    /// requirement key this is, counted in declaration order.
    pub occurrence: u32,
}

impl ObligationKey {
    /// The identity of the requirement declaration that produced the
    /// obligation: its key with the property replaced by its family
    /// (§9.2). A result replay obligation is the result half of an
    /// idempotency requirement, so both name one declaration.
    pub fn requirement(&self) -> RequirementIdentity {
        RequirementIdentity {
            subject: self.subject.clone(),
            family: self.property.family(),
            requirement_key: self.requirement_key.clone(),
            occurrence: self.occurrence,
        }
    }
}

impl fmt::Display for ObligationKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let key = self
            .requirement_key
            .iter()
            .map(|component| format!("{}.{}", component.source.id(), component.path))
            .collect::<Vec<_>>()
            .join(", ");

        write!(f, "{}({key}) on {}", self.property, self.subject)?;

        if self.occurrence > 0 {
            write!(f, " #{}", self.occurrence)?;
        }

        Ok(())
    }
}

/// What an obligation is a property of: an operation, or one inline
/// transaction of it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObligationSubject {
    Operation { operation: Id },
    Transaction { operation: Id, transaction: Id },
}

impl fmt::Display for ObligationSubject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Operation { operation } => write!(f, "{operation}"),
            Self::Transaction { transaction, .. } => write!(f, "{transaction}"),
        }
    }
}

/// The property an obligation key names. The report's `Property` also
/// admits a custom property, which no declared requirement produces, so
/// a key carries this closed set and spells it as a plain name (§14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObligationProperty {
    TransactionSerializability,
    TransactionOrdering,
    Idempotency,
    Recoverability,
    ResultReplay,
}

impl ObligationProperty {
    /// The requirement family that declares obligations of this
    /// property.
    pub fn family(self) -> RequirementFamily {
        match self {
            Self::TransactionSerializability => RequirementFamily::TransactionSerializability,
            Self::TransactionOrdering => RequirementFamily::TransactionOrdering,
            Self::Idempotency | Self::ResultReplay => RequirementFamily::Idempotency,
            Self::Recoverability => RequirementFamily::Recoverability,
        }
    }
}

impl fmt::Display for ObligationProperty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TransactionSerializability => "SerializableBy",
            Self::TransactionOrdering => "OrderedBy",
            Self::Idempotency => "Idempotency",
            Self::Recoverability => "Recoverability",
            Self::ResultReplay => "ResultReplay",
        })
    }
}

/// The four requirement families a model declares (§9.3). Result
/// replay is not one of them: it is the result half of an idempotency
/// requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementFamily {
    TransactionSerializability,
    TransactionOrdering,
    Idempotency,
    Recoverability,
}

impl fmt::Display for RequirementFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TransactionSerializability => "SerializableBy",
            Self::TransactionOrdering => "OrderedBy",
            Self::Idempotency => "Idempotency",
            Self::Recoverability => "Recoverability",
        })
    }
}

/// The identity of one declared requirement: the obligation key of
/// what it produces, with the property replaced by its family (§9.2).
/// It names the `requirement` member of the declaration partition.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequirementIdentity {
    pub subject: ObligationSubject,
    pub family: RequirementFamily,
    pub requirement_key: Vec<ValueRef>,
    pub occurrence: u32,
}

impl fmt::Display for RequirementIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}({}) on {}",
            self.family,
            list(self.requirement_key.iter().map(value)),
            self.subject
        )?;

        if self.occurrence > 0 {
            write!(f, " #{}", self.occurrence)?;
        }

        Ok(())
    }
}

/// The keys of a list of requirements on one subject, in declaration
/// order: each counts the earlier requirements whose key it shares.
pub fn requirement_keys(
    subject: &ObligationSubject,
    property: ObligationProperty,
    keys: impl IntoIterator<Item = Vec<ValueRef>>,
) -> Vec<ObligationKey> {
    let mut seen: Vec<Vec<ValueRef>> = Vec::new();

    keys.into_iter()
        .map(|requirement_key| {
            let occurrence = seen
                .iter()
                .filter(|earlier| **earlier == requirement_key)
                .count();

            seen.push(requirement_key.clone());

            ObligationKey {
                subject: subject.clone(),
                property,
                requirement_key,
                occurrence: u32::try_from(occurrence).unwrap_or(u32::MAX),
            }
        })
        .collect()
}

/// The keys of an operation's idempotency requirements, in declaration
/// order, with the given property: `Idempotency` for their side-effect
/// obligations, `ResultReplay` for their result halves.
pub fn idempotency_keys(
    operation_id: &Id,
    operation: &crate::spec::Operation,
    property: ObligationProperty,
) -> Vec<ObligationKey> {
    requirement_keys(
        &ObligationSubject::Operation {
            operation: operation_id.clone(),
        },
        property,
        operation
            .requirements
            .idempotency
            .iter()
            .map(|requirement| requirement.key.components.clone()),
    )
}

/// The keys of an operation's recoverability requirements, in
/// declaration order.
pub fn recoverability_keys(
    operation_id: &Id,
    operation: &crate::spec::Operation,
) -> Vec<ObligationKey> {
    requirement_keys(
        &ObligationSubject::Operation {
            operation: operation_id.clone(),
        },
        ObligationProperty::Recoverability,
        operation
            .requirements
            .recoverability
            .iter()
            .map(|requirement| requirement.key.components.clone()),
    )
}

/// The keys of a transaction's serializability and ordering
/// requirements, in declaration order.
pub fn transaction_keys(
    operation: &Id,
    transaction: &crate::spec::Transaction,
    property: ObligationProperty,
) -> Vec<ObligationKey> {
    let subject = ObligationSubject::Transaction {
        operation: operation.clone(),
        transaction: transaction.id.clone(),
    };

    match property {
        ObligationProperty::TransactionSerializability => requirement_keys(
            &subject,
            property,
            transaction
                .requirements
                .serializability
                .iter()
                .map(|requirement| vec![requirement.key.clone()]),
        ),

        ObligationProperty::TransactionOrdering => requirement_keys(
            &subject,
            property,
            transaction
                .requirements
                .ordering
                .iter()
                .map(|requirement| vec![requirement.key.clone()]),
        ),

        ObligationProperty::Idempotency
        | ObligationProperty::Recoverability
        | ObligationProperty::ResultReplay => Vec::new(),
    }
}

// ---------------------------------------------------------------------
// Facts
// ---------------------------------------------------------------------

/// One fact a proof can consume, in the model's vocabulary: a declared
/// fact, or a model-wide query result (§10.5, §10.9).
///
/// The enum is closed, one variant per kind, and every field is in
/// canonical DSL form: ids as strings, field paths as component lists,
/// value references and object selectors in their tagged form, enums in
/// snake case, sets as sorted lists. Two facts are the same fact
/// exactly when their canonical forms are equal. The declarations a
/// fact was read from are its anchors, held beside it in a
/// [`BasisFact`] and never part of its identity, so a fact that still
/// holds after its anchor changed elsewhere is the same fact.
///
/// No fact names a step, a step location, or a path: positions are
/// presentation, and a fact must survive the steps around it being
/// renumbered (§10.3). Where a proof names a step, the fact names what
/// the step does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Fact {
    /// The transaction's declared isolation.
    Isolation {
        operation: Id,
        transaction: Id,
        level: TransactionIsolation,
    },

    /// A lock step: its selector is the step's canonical selector, so
    /// two locks one transaction takes on two instances of one object
    /// are two facts.
    Lock {
        operation: Id,
        transaction: Id,
        data_model: Id,
        object: Id,
        selector: ObjectSelector,
        mode: LockMode,
    },

    /// An atomic conditional mutation, credited as commit-order
    /// evidence for the reason `coverage` names.
    GuardedMutation {
        operation: Id,
        transaction: Id,
        data_model: Id,
        object: Id,
        mechanism: Mechanism,
        coverage: Coverage,
        compared_fields: BTreeSet<FieldPath>,
    },

    /// A managed cursor field, advanced only under `rule`.
    Cursor {
        data_model: Id,
        object: Id,
        field: FieldPath,
        rule: CursorAdvanceRule,
    },

    /// A managed fence field.
    Fence {
        data_model: Id,
        object: Id,
        field: FieldPath,
    },

    /// The conflict closure of a transaction under a serializability
    /// requirement: a query result, read from every member.
    ConflictClosure {
        root: TransactionId,
        key: ValueRef,
        members: BTreeSet<TransactionId>,
    },

    /// The cyclic components of a closure's dependency graph: a query
    /// result naming which dependencies needed commit-order evidence.
    CyclicComponents {
        root: TransactionId,
        key: ValueRef,
        components: BTreeSet<BTreeSet<TransactionId>>,
    },

    /// No transaction of the model deletes the object: a query result,
    /// whose truth an added deleter falsifies.
    NoDeleters { data_model: Id, object: Id },

    /// The operation declares a requirement of `property` keyed from
    /// `input`: a query over its requirements. A consumer or request
    /// target that declares none fails it, and the obstacle states it
    /// as missing, so that a requirement added or removed on either
    /// side explains the change (§12.1). `property` is `idempotency` or
    /// `result_replay`.
    KeyedRequirement {
        operation: Id,
        input: Id,
        property: ObligationProperty,
    },

    /// The triggering input admits no message schema.
    AdmitsNoMessages { operation: Id, input: Id },

    /// No path of the program is admitted for the input.
    NoAdmittedPaths { operation: Id, input: Id },

    /// No admitted path returns a result for the input.
    ReturnsNoResult { operation: Id, input: Id },

    /// A transaction ending a path of the input is never re-encountered.
    Terminal { operation: Id, input: Id },

    /// A value reference is replay-stable relative to the governing
    /// key, by `rule`.
    StableRoot {
        operation: Id,
        rule: RootRule,
        root: ValueRef,
    },

    /// A subscription's declared delivery: the one L1 fact a proof
    /// consumes.
    Delivery {
        operation: Id,
        input: Id,
        semantics: DeliverySemantics,
    },

    /// A channel's message identity for one schema: the ordered fields
    /// holding it.
    MessageIdentity {
        channel: Channel,
        schema: Id,
        fields: Vec<FieldPath>,
    },

    /// A transaction's keyed commit deduplication.
    DeduplicatedBy {
        operation: Id,
        transaction: Id,
        key: Vec<ValueRef>,
    },

    /// A transaction's body re-executes to the same logical state.
    NaturallyReplayable { operation: Id, transaction: Id },

    /// How an artifact a transaction establishes reaches a retry.
    ArtifactRoute {
        operation: Id,
        transaction: Id,
        artifact: Id,
        route: ArtifactRoute,
    },

    /// An outbox durably re-drives its consuming input.
    OutboxRedrive {
        operation: Id,
        input: Id,
        data_model: Id,
        outbox: Id,
    },

    /// A request effect declares `retry: may_repeat`.
    RetryMayRepeat { site: Site },

    /// An external boundary's declared guarantee.
    ExternalGuarantee {
        operation: Id,
        effect: Id,
        guarantee: Guarantee,
        identity: ExternalIdentity,
    },

    /// A producer's declared propagation of its key onto a message's
    /// identity fields.
    Propagation {
        producer: Site,
        schema: Id,
        channel: Channel,
    },

    /// The proof rests on another obligation, whose own basis is not
    /// inlined (§10.6).
    ReliesOn { obligation: ObligationKey },

    /// A configuration requirement of a chosen realization (Part VI
    /// §12). Not a proof fact: no basis contains one.
    Realization {
        declaration: TextualKey,
        entry: EntryRef,
        realizes: TextualKey,
        requirement: String,
    },
}

impl Fact {
    /// The fact's semantic layer: L1 for runtime declarations, L0
    /// otherwise (§10.2).
    pub fn layer(&self) -> Layer {
        match self {
            Self::Delivery { .. } => Layer::L1,

            Self::Isolation { .. }
            | Self::Lock { .. }
            | Self::GuardedMutation { .. }
            | Self::Cursor { .. }
            | Self::Fence { .. }
            | Self::ConflictClosure { .. }
            | Self::CyclicComponents { .. }
            | Self::NoDeleters { .. }
            | Self::KeyedRequirement { .. }
            | Self::AdmitsNoMessages { .. }
            | Self::NoAdmittedPaths { .. }
            | Self::ReturnsNoResult { .. }
            | Self::Terminal { .. }
            | Self::StableRoot { .. }
            | Self::MessageIdentity { .. }
            | Self::DeduplicatedBy { .. }
            | Self::NaturallyReplayable { .. }
            | Self::ArtifactRoute { .. }
            | Self::OutboxRedrive { .. }
            | Self::RetryMayRepeat { .. }
            | Self::ExternalGuarantee { .. }
            | Self::Propagation { .. }
            | Self::ReliesOn { .. }
            | Self::Realization { .. } => Layer::L0,
        }
    }

    /// The fact's canonical form as text, which is its identity: two
    /// facts are the same fact exactly when these are equal (§10.9).
    /// Maps are written with their keys sorted, so the text depends on
    /// the durable form alone, never on the order fields are declared
    /// in here.
    pub fn canonical(&self) -> String {
        // Every field is a string, an integer, a sequence, or a
        // string-keyed map, so serialization cannot fail.
        durable::canonical_text(&serde_json::to_value(self).expect("a fact serializes"))
    }
}

/// The fact in one line, in the functional shorthand the specs write
/// it in: `isolation(tx.apply_payment) = serializable`. Presentation
/// only, for a diff's human output and a work item's packet; the
/// canonical form is the fact's identity. A transaction is written by
/// its id alone, as a reader names it.
impl fmt::Display for Fact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Isolation {
                transaction, level, ..
            } => write!(f, "isolation({transaction}) = {}", snake(level)),

            Self::Lock {
                transaction,
                object,
                selector,
                mode,
                ..
            } => write!(
                f,
                "lock({transaction}, {object} where {}, {})",
                predicate(&selector.predicate),
                snake(mode)
            ),

            Self::GuardedMutation {
                transaction,
                object,
                mechanism,
                coverage,
                compared_fields,
                ..
            } => {
                write!(
                    f,
                    "guarded_mutation({transaction}, {object}, {}, {coverage}",
                    snake(mechanism)
                )?;

                if !compared_fields.is_empty() {
                    write!(f, ", comparing {}", list(compared_fields))?;
                }

                f.write_str(")")
            }

            Self::Cursor {
                object,
                field,
                rule,
                ..
            } => write!(f, "cursor({object}.{field}) = {rule}"),

            Self::Fence { object, field, .. } => write!(f, "fence({object}.{field})"),

            Self::ConflictClosure { root, key, members } => write!(
                f,
                "conflict_closure({}, {}) = {{{}}}",
                root.transaction,
                value(key),
                list(members.iter().map(|member| &member.transaction))
            ),

            Self::CyclicComponents {
                root,
                key,
                components,
            } => write!(
                f,
                "cyclic_components({}, {}) = {{{}}}",
                root.transaction,
                value(key),
                list(components.iter().map(|component| format!(
                    "{{{}}}",
                    list(component.iter().map(|member| &member.transaction))
                )))
            ),

            Self::NoDeleters { object, .. } => write!(f, "no_deleters({object})"),

            Self::KeyedRequirement {
                operation,
                input,
                property,
            } => write!(
                f,
                "keyed_requirement({operation}, {input}, {})",
                snake(property)
            ),
            Self::AdmitsNoMessages { input, .. } => write!(f, "admits_no_messages({input})"),
            Self::NoAdmittedPaths { input, .. } => write!(f, "no_admitted_paths({input})"),
            Self::ReturnsNoResult { input, .. } => write!(f, "returns_no_result({input})"),
            Self::Terminal { input, .. } => write!(f, "terminal({input})"),

            Self::StableRoot { rule, root, .. } => {
                write!(f, "stable_root({}) by {}", value(root), snake(rule))
            }

            Self::Delivery {
                input, semantics, ..
            } => write!(f, "delivery({input}) = {}", snake(semantics)),

            Self::MessageIdentity {
                channel,
                schema,
                fields,
            } => write!(
                f,
                "message_identity({channel}, {schema}) = ({})",
                list(fields)
            ),

            Self::DeduplicatedBy {
                transaction, key, ..
            } => write!(
                f,
                "deduplicated_by({transaction}) = ({})",
                list(key.iter().map(value))
            ),

            Self::NaturallyReplayable { transaction, .. } => {
                write!(f, "naturally_replayable({transaction})")
            }

            Self::ArtifactRoute {
                transaction,
                artifact,
                route,
                ..
            } => write!(
                f,
                "artifact_route({transaction}, {artifact}) = {}",
                snake(route)
            ),

            Self::OutboxRedrive { input, outbox, .. } => {
                write!(f, "outbox_redrive({input}, {outbox})")
            }

            Self::RetryMayRepeat { site } => write!(f, "retry_may_repeat({site})"),

            Self::ExternalGuarantee {
                effect, guarantee, ..
            } => write!(f, "external_guarantee({effect}) = {}", snake(guarantee)),

            Self::Propagation {
                producer,
                schema,
                channel,
            } => write!(f, "propagation({producer}, {schema}, {channel})"),

            Self::ReliesOn { obligation } => write!(f, "relies_on({obligation})"),

            Self::Realization {
                declaration,
                entry,
                requirement,
                ..
            } => write!(f, "realization({declaration}, {entry}, {requirement})"),
        }
    }
}

impl fmt::Display for Coverage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Atomic => f.write_str("atomic"),
            Self::HeldProtection => f.write_str("held_protection"),
            Self::ObservedState { read } => write!(f, "observed_state({read})"),

            Self::ObservedVersion {
                read,
                version_field,
            } => write!(f, "observed_version({read}, {version_field})"),

            Self::LockedReader { selector, mode } => write!(
                f,
                "locked_reader({} where {}, {})",
                selector.object,
                predicate(&selector.predicate),
                snake(mode)
            ),
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Topic { topic } => write!(f, "{topic}"),
            Self::Outbox { outbox, .. } => write!(f, "{outbox}"),
        }
    }
}

impl fmt::Display for Site {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EffectSite { effect, .. } => write!(f, "{effect}"),

            Self::TransitionEffect {
                transition, effect, ..
            } => write!(f, "{transition}.{effect}"),
        }
    }
}

/// A unit enum value as its durable spelling.
fn snake<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => text,
        _ => String::new(),
    }
}

fn value(value: &ValueRef) -> String {
    format!("{}.{}", value.source.id(), value.path)
}

fn list<T: fmt::Display>(items: impl IntoIterator<Item = T>) -> String {
    items
        .into_iter()
        .map(|item| item.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn predicate(selected: &SelectorPredicate) -> String {
    match selected {
        SelectorPredicate::All => "all".to_string(),

        SelectorPredicate::Eq { field, value: to } => match to {
            SelectorValue::Value(root) => format!("{field} = {}", value(root)),
            SelectorValue::Literal(literal) => format!("{field} = {}", snake_literal(literal)),
        },

        SelectorPredicate::And { predicates } => predicates
            .iter()
            .map(predicate)
            .collect::<Vec<_>>()
            .join(" and "),
    }
}

fn snake_literal(literal: &Literal) -> String {
    match literal {
        Literal::String(text) => text.clone(),
        Literal::Bool(flag) => flag.to_string(),
        Literal::Int(number) => number.to_string(),
    }
}

/// An inline transaction by its position-free identity: the declaring
/// operation and the transaction's id.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionId {
    pub operation: Id,
    pub transaction: Id,
}

/// The mechanism of a guarded mutation, by the DSL step that performs
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mechanism {
    CompareAndSet,
    Transition,
    AdvanceCursor,
    Fence,
    Upsert,
}

/// Why a guarded mutation covers what it is credited for, naming reads
/// by their binding ids and never by step (§10.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Coverage {
    /// The covered access is part of the mutation's own statement.
    Atomic,

    /// The covered access follows the mutation, under the write
    /// protection it holds to commit.
    HeldProtection,

    /// The mutation compares fields against the values `read` observed.
    ObservedState { read: Id },

    /// The mutation compares the version `read` observed.
    ObservedVersion { read: Id, version_field: FieldPath },

    /// The reader holds this lock from before its observation to
    /// commit, and the mutation must acquire the instance's write
    /// protection, which the lock withholds.
    LockedReader {
        selector: ObjectSelector,
        mode: LockMode,
    },
}

/// The rule that makes a root replay-stable. A root into a request
/// effect's replay-consistent result is not a rule here: it relies on
/// the target's result replay obligation instead (§10.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootRule {
    KeyComponent,
    IdentifiedPayload,
    RecoveredArtifact,
    ReconstructedArtifact,
    ReplayStableExternalResult,
}

/// How an artifact reaches a retry (§17 of the semantics contract).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactRoute {
    /// From the single keyed commit of the establishing transaction.
    Recovered,

    /// By naturally replaying the establishing transaction.
    Reconstructed,
}

/// An external boundary guarantee a proof consumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Guarantee {
    /// Duplicate applications of one interaction are indistinguishable
    /// from one.
    Idempotent,

    /// An application changes no modeled external state.
    SideEffectFree,

    /// Every application of one interaction observes its terminal
    /// result.
    ReplayStable,
}

/// A message channel: a topic, or a data model's outbox.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Channel {
    Topic { topic: Id },
    Outbox { data_model: Id, outbox: Id },
}

/// Where an effect executes: an operation's effect site, or a state
/// machine transition's side effect.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Site {
    EffectSite {
        operation: Id,
        effect: Id,
    },

    TransitionEffect {
        machine: Id,
        transition: Id,
        effect: Id,
    },
}

/// A catalogue entry at one version, written `id@version` (Part VI
/// §12).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntryRef {
    pub id: String,
    pub version: u32,
}

impl EntryRef {
    /// Parses `id@version`: a non-empty id with no `@` or whitespace,
    /// and a version written in canonical decimal.
    pub fn parse(text: &str) -> Option<Self> {
        let (id, version) = text.rsplit_once('@')?;

        let well_formed = !id.is_empty()
            && !id.contains('@')
            && !id.chars().any(char::is_whitespace)
            && !version.is_empty()
            && version.bytes().all(|byte| byte.is_ascii_digit())
            && (version == "0" || !version.starts_with('0'));

        if !well_formed {
            return None;
        }

        Some(Self {
            id: id.to_string(),
            version: version.parse().ok()?,
        })
    }
}

impl fmt::Display for EntryRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.id, self.version)
    }
}

impl Serialize for EntryRef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for EntryRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;

        Self::parse(&text).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "`{text}` is not a catalogue entry written `id@version`"
            ))
        })
    }
}

/// The semantic layer of a fact (§10.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    L0,
    L1,
}

impl fmt::Display for Layer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::L0 => "l0",
            Self::L1 => "l1",
        })
    }
}

// ---------------------------------------------------------------------
// Bases and blocked-on sets
// ---------------------------------------------------------------------

/// One fact with the declarations its truth was read from (§10.2).
///
/// `anchors` take no part in the fact's identity. They are what a diff
/// explains a changed verdict by, so a fact anchors every declaration
/// whose content its truth was read from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BasisFact {
    pub fact: Fact,
    pub anchors: BTreeSet<DeclarationKey>,
    pub layer: Layer,
}

/// What an unproven verdict is blocked on (§10.7): the facts its
/// obstacles found present but insufficient, the facts they found
/// missing, and every declaration they read.
///
/// A missing fact is stated only where a gap names the premise its
/// route lacks exactly: a lock with the access's selector and mode, an
/// at-least-once delivery, the absence of deleters, a naturally
/// replayable body. Elsewhere the remedy is a choice the gap does not
/// make, and the obstacle contributes its anchors alone. The set is
/// what lets a regression be explained from the head side and an
/// improvement from the base side.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockedOn {
    pub present: Vec<BasisFact>,
    pub missing: Vec<BasisFact>,
    pub anchors: BTreeSet<DeclarationKey>,
}

impl BlockedOn {
    pub fn is_empty(&self) -> bool {
        self.present.is_empty() && self.missing.is_empty() && self.anchors.is_empty()
    }
}

/// Facts collected during extraction, in their canonical order, each
/// with the union of the anchors it was found with.
#[derive(Debug, Clone, Default)]
pub(crate) struct FactSet {
    facts: BTreeMap<String, BasisFact>,
}

impl FactSet {
    pub(crate) fn insert(&mut self, fact: Fact, anchors: impl IntoIterator<Item = DeclarationKey>) {
        let entry = self
            .facts
            .entry(fact.canonical())
            .or_insert_with(|| BasisFact {
                layer: fact.layer(),
                fact,
                anchors: BTreeSet::new(),
            });

        entry.anchors.extend(anchors);
    }

    pub(crate) fn into_facts(self) -> Vec<BasisFact> {
        self.facts.into_values().collect()
    }
}
