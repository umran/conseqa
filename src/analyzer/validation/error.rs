use crate::analyzer::{
    Diagnostic, DiagnosticCode, Evidence, IdDeclaration, Severity, ValidationCode,
};
use crate::spec::{CursorAdvanceRule, FieldPath, Id, ResultVariant, StepLocation, ValueRef};

use super::{InputKind, ReferenceKind};

/// Which transaction requirement family a diagnostic concerns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionRequirementFamily {
    Serializability,
    Ordering,
}

impl std::fmt::Display for TransactionRequirementFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Serializability => "serializability",
            Self::Ordering => "ordering",
        })
    }
}

/// Why a value is not available when a transaction begins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryUnavailability {
    /// The value is a read performed inside the transaction itself.
    TransactionRead { read: Id },

    /// A transaction artifact no transaction on every reaching path
    /// establishes.
    ArtifactNotAvailable { artifact: Id },

    /// A result binding no effect-executing step on every reaching
    /// path binds.
    ResultNotBound { result: Id },

    /// A result payload referenced outside the match arm that selects
    /// it.
    ResultPayloadOutOfScope { result: Id },
}

impl std::fmt::Display for EntryUnavailability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TransactionRead { read } => write!(
                f,
                "it is transaction read `{read}`, which is performed inside the transaction \
                 and so does not exist when it begins"
            ),

            Self::ArtifactNotAvailable { artifact } => write!(
                f,
                "transaction artifact `{artifact}` is not established on every path reaching \
                 the step"
            ),

            Self::ResultNotBound { result } => write!(
                f,
                "result `{result}` is not bound on every path reaching the step"
            ),

            Self::ResultPayloadOutOfScope { result } => write!(
                f,
                "the payload of result `{result}` is referenced outside the match arm that \
                 selects it"
            ),
        }
    }
}

/// The semantic role a managed monotonic field plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ManagedRole {
    Version,
    Cursor { rule: CursorAdvanceRule },
    Fence,
}

impl std::fmt::Display for ManagedRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Version => f.write_str("version"),
            Self::Cursor { rule } => write!(f, "cursor ({rule})"),
            Self::Fence => f.write_str("fence"),
        }
    }
}

/// Why a declared version field is not a valid one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionFieldDefect {
    Unresolved,
    Optional,
    NotInt { found: String },
    IdentityField,
}

impl std::fmt::Display for VersionFieldDefect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unresolved => f.write_str("it does not resolve against the object's schema"),
            Self::Optional => f.write_str("it is optional, and a version token must always exist"),
            Self::NotInt { found } => write!(f, "it is {found}, and a version token must be int"),
            Self::IdentityField => {
                f.write_str("it is part of the object's identity, which a version never is")
            }
        }
    }
}

/// Why a managed field, or the value driving it, has the wrong type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedFieldDefect {
    Unresolved,
    Optional,
    NotOrderedScalar { found: String },
    NotInt { found: String },
    ValueTypeMismatch { expected: String, found: String },
}

impl std::fmt::Display for ManagedFieldDefect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unresolved => {
                f.write_str("the field does not resolve against the object's schema")
            }
            Self::Optional => {
                f.write_str("the field is optional, and a managed position must always exist")
            }
            Self::NotOrderedScalar { found } => write!(
                f,
                "the field is {found}, and a managed position must be an ordered scalar: int, \
                 decimal, or timestamp"
            ),
            Self::NotInt { found } => {
                write!(
                    f,
                    "the field is {found}, and a successor cursor must be int"
                )
            }
            Self::ValueTypeMismatch { expected, found } => write!(
                f,
                "the incoming value is {found}, but the field is {expected}"
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    DuplicateId {
        id: Id,
        first: IdDeclaration,
        second: IdDeclaration,
    },

    UnknownReference {
        subject: Id,
        reference: Id,
        expected: ReferenceKind,
    },

    InvalidReferenceKind {
        subject: Id,
        reference: Id,
        expected: ReferenceKind,
        actual: ReferenceKind,
    },

    InvalidReferenceOwner {
        subject: Id,
        reference: Id,
        expected_owner: Id,
        actual_owner: Option<Id>,
    },

    InvalidFieldPath {
        subject: Id,
        schema: Id,
        path: FieldPath,
    },

    /// A ValueRef attempts to dereference a source for which
    /// the model defines no structural payload schema.
    ValueSourceHasNoSchema {
        subject: Id,
        source: Id,
    },

    /// A ValueRef names a source that the invocations evaluating it
    /// cannot observe, such as another operation's input.
    ValueSourceOutOfScope {
        subject: Id,
        source: Id,
        owner: Id,
    },

    FragmentCycle {
        cycle: Vec<Id>,
    },

    DataObjectSchemaNotCanonical {
        object: Id,
        schema: Id,
    },

    SubscriptionMessageNotOnTopic {
        input: Id,
        topic: Id,
        schema: Id,
    },

    PublicationEffectMessageNotOnTopic {
        effect: Id,
        topic: Id,
        schema: Id,
    },

    /// A message-identity mapping names a schema the topic does not
    /// carry.
    MessageIdentitySchemaNotOnTopic {
        topic: Id,
        schema: Id,
    },

    /// A message-identity mapping declares an empty identity tuple.
    EmptyMessageIdentity {
        topic: Id,
        schema: Id,
    },

    /// Message-identity tuple positions correspond across schemas, so
    /// every mapped tuple must have the same arity.
    MessageIdentityArityMismatch {
        topic: Id,
        schema: Id,
        expected: usize,
        actual: usize,
    },

    /// A request input declares a keyed identity with no fields.
    EmptyRequestIdentity {
        input: Id,
    },

    /// A transaction requirement key is not available when the
    /// transaction begins: it is a read the transaction itself
    /// performs, or an artifact or result not definitely established
    /// on every path reaching the step.
    TransactionRequirementKeyUnavailable {
        operation: Id,
        location: StepLocation,
        transaction: Id,
        family: TransactionRequirementFamily,
        requirement: usize,
        key: ValueRef,
        reason: EntryUnavailability,
    },

    /// An ordering requirement's position is not available when the
    /// transaction begins.
    TransactionOrderingPositionUnavailable {
        operation: Id,
        location: StepLocation,
        transaction: Id,
        requirement: usize,
        position: ValueRef,
        reason: EntryUnavailability,
    },

    /// An ordering requirement's position does not resolve to a
    /// non-optional ordered scalar — `int`, `decimal`, or `timestamp`.
    TransactionOrderingPositionNotOrderedScalar {
        operation: Id,
        transaction: Id,
        requirement: usize,
        position: ValueRef,
        found: String,
    },

    /// A transaction containing a rejecting step — a transition, a
    /// version validation, a cursor advance, or a fence — is executed
    /// without a `rejected` block.
    MissingTransactionRejectedArm {
        operation: Id,
        location: StepLocation,
        transaction: Id,
        step: usize,
    },

    /// A transaction with no rejecting step is executed with a
    /// `rejected` block that control can never enter.
    UnexpectedTransactionRejectedArm {
        operation: Id,
        location: StepLocation,
        transaction: Id,
    },

    /// A transition-scoped outbox effect names an outbox no data model
    /// declares.
    UnknownTransitionOutbox {
        machine: Id,
        transition: Id,
        effect: Id,
        outbox: Id,
    },

    /// A transition-scoped outbox effect admits a schema that is not
    /// declared, or that the outbox does not admit.
    InvalidTransitionOutboxSchema {
        machine: Id,
        transition: Id,
        effect: Id,
        outbox: Id,
        schema: Id,
    },

    /// A transition-scoped outbox effect targets an outbox outside the
    /// data model that owns the machine's subject object, so the
    /// admission could not be atomic with the transition.
    TransitionOutboxOutsideDataModel {
        machine: Id,
        transition: Id,
        effect: Id,
        outbox: Id,
        data_model: Id,
    },

    /// A `StateTransition` step's `effects` keys do not exactly match
    /// the outbox effects declared by the applied transition, so some
    /// admission has no message derivation or a derivation names no
    /// declared admission.
    InvalidTransitionOutboxDerivation {
        transaction: Id,
        transition: Id,
        missing: Vec<Id>,
        unexpected: Vec<Id>,
    },

    /// An object's declared version field is not a non-optional `int`
    /// outside its identity.
    InvalidObjectVersionField {
        object: Id,
        field: FieldPath,
        defect: VersionFieldDefect,
    },

    /// An ordinary write names an object's version field, which only
    /// the version protocol may assign.
    DirectWriteToVersionField {
        transaction: Id,
        step: usize,
        object: Id,
        field: FieldPath,
    },

    /// A write or transition of a live versioned instance is not
    /// accompanied by a `bump_version` of the same instance.
    MissingVersionBump {
        transaction: Id,
        step: usize,
        object: Id,
    },

    /// One transaction bumps one selected instance more than once.
    DuplicateVersionBump {
        transaction: Id,
        step: usize,
        object: Id,
    },

    /// A `validate_version` step's expected version is not a preceding
    /// read of the same instance's declared version field.
    VersionValidationWithoutObservedVersion {
        transaction: Id,
        step: usize,
        object: Id,
    },

    /// A `validate_version` or `bump_version` step targets an object
    /// that declares no version.
    VersionProtocolOnUnversionedObject {
        transaction: Id,
        step: usize,
        object: Id,
    },

    /// One field is used in two managed roles — a version, a cursor
    /// under some rule, a fence — and a managed field has exactly one.
    ManagedFieldRoleConflict {
        object: Id,
        field: FieldPath,
        first: ManagedRole,
        second: ManagedRole,
    },

    /// An ordinary write names a cursor or fence field, which only its
    /// protocol step may assign.
    DirectWriteToManagedField {
        transaction: Id,
        step: usize,
        object: Id,
        field: FieldPath,
        role: ManagedRole,
    },

    /// A cursor or fence field, or the value driving it, is not of the
    /// type its role requires.
    InvalidManagedFieldType {
        object: Id,
        field: FieldPath,
        role: ManagedRole,
        defect: ManagedFieldDefect,
    },

    /// A `return` names an error class the request's result contract
    /// does not declare.
    UnknownResultErrorClass {
        operation: Id,
        location: StepLocation,
        request: Id,
        error: Id,
    },

    /// A `match_result` has no arm for an error class the matched
    /// result's contract declares.
    MissingResultErrorArm {
        operation: Id,
        location: StepLocation,
        result: Id,
        error: Id,
    },

    /// A `match_result` has an arm for an error class the matched
    /// result's contract does not declare.
    UnexpectedResultErrorArm {
        operation: Id,
        location: StepLocation,
        result: Id,
        error: Id,
    },

    /// A router declares a routing block whose key tuple is empty, so
    /// it names no routing domain.
    EmptyRoutingKey {
        router: Id,
    },

    /// A subscription dispatch routes by `grouping_key`, but no keyed
    /// grouping is in effect at either scope for that key to name.
    RoutingWithoutGrouping {
        operation: Id,
        input: Id,
        topic: Id,
    },

    /// A grouping key maps a schema the topic does not carry.
    GroupingKeySchemaNotOnTopic {
        subject: Id,
        topic: Id,
        schema: Id,
    },

    /// A grouping key leaves a carried schema unmapped, so messages of
    /// it would belong to no group.
    GroupingKeyMissingSchema {
        subject: Id,
        topic: Id,
        schema: Id,
    },

    /// A grouping key maps a schema to an empty tuple.
    EmptyGroupingKey {
        subject: Id,
        schema: Id,
    },

    /// Grouping-key tuple positions correspond across schemas, so
    /// every mapped tuple must have the same arity.
    GroupingKeyArityMismatch {
        subject: Id,
        schema: Id,
        expected: usize,
        actual: usize,
    },

    /// `ordering: within_group` is declared where no keyed grouping is
    /// declared at the same scope, so no domain exists for the
    /// guarantee to be interpreted over.
    WithinGroupWithoutGrouping {
        subject: Id,
    },

    /// A topic and one of its subscriptions both declare transport
    /// semantics. The two scopes are exclusive.
    TransportSemanticsAtBothScopes {
        topic: Id,
        operation: Id,
        input: Id,
    },

    /// Two routers serve one request boundary. The initial model
    /// admits at most one, so the assignment of a boundary to a pool
    /// is unambiguous.
    DuplicateRouterForBoundary {
        first: Id,
        second: Id,
        operation: Id,
        input: Id,
    },

    /// A storage layout declares an empty partition key, so it
    /// identifies no partition.
    EmptyPartitionKey {
        layout: Id,
    },

    /// Two storage layouts map one data object. V1 admits at most one
    /// primary layout per object.
    DuplicateStorageLayoutForObject {
        first: Id,
        second: Id,
        data_model: Id,
        object: Id,
    },

    TransactionObjectOutsideDataModel {
        transaction: Id,
        data_model: Id,
        object: Id,
    },

    TransactionMissingDataModel {
        transaction: Id,
        object: Id,
    },

    /// A ValueRef names a transaction-read result outside the
    /// transaction execution that produces it.
    TransactionReadOutsideTransaction {
        subject: Id,
        read: Id,
    },

    /// A ValueRef names a transaction-read result that does not
    /// precede its use in transaction program order.
    TransactionReadOutOfOrder {
        transaction: Id,
        read: Id,
    },

    /// A ValueRef names a field the read did not select.
    TransactionReadFieldNotSelected {
        transaction: Id,
        read: Id,
        path: FieldPath,
    },

    StateTransitionSubjectMismatch {
        transaction: Id,
        machine: Id,
        expected_object: Id,
        actual_object: Id,
    },

    /// A `StateTransition` step's `effect_intents` keys do not exactly
    /// match the side effects declared by the applied transition.
    TransitionEffectIntentsMismatch {
        transaction: Id,
        transition: Id,
        missing: Vec<Id>,
        unexpected: Vec<Id>,
    },

    EmptyObjectIdentity {
        object: Id,
    },

    InvalidInputKind {
        subject: Id,
        input: Id,
        expected: InputKind,
        actual: InputKind,
    },

    /// Some reachable path through the operation program falls off the
    /// end of its last step without reaching a `return` or `complete`
    /// terminal.
    ProgramNotTerminated {
        operation: Id,
    },

    /// A program step follows a terminal — or a decision whose every
    /// arm terminates — in its block, so no invocation reaches it.
    UnreachableProgramStep {
        operation: Id,
        location: StepLocation,
    },

    /// A program point consumes a transaction artifact — a transaction
    /// output or an effect intent — that is not definitely established
    /// or recovered on every path reaching it.
    TransactionArtifactNotAvailable {
        operation: Id,
        location: StepLocation,
        artifact: Id,
        consumer: ProgramUse,
    },

    /// A result binding is matched or referenced where no
    /// effect-executing step on every path reaching the point has bound
    /// it.
    EffectResultNotBound {
        operation: Id,
        location: StepLocation,
        result: Id,
        consumer: ProgramUse,
    },

    /// A variant payload of a bound result is referenced outside the
    /// arm of a `match_result` on that binding that selects the
    /// variant.
    EffectResultVariantOutOfScope {
        operation: Id,
        location: StepLocation,
        result: Id,
        variant: ResultVariant,
        consumer: ProgramUse,
    },

    /// A step binds the result of an effect whose contract has no
    /// synchronous result: a publication, or an external effect that
    /// declares none.
    EffectHasNoResult {
        operation: Id,
        location: StepLocation,
        effect: Id,
        result: Id,
    },

    /// An external effect declares `identical_per_identity` without a
    /// keyed interaction identity: the guarantee is quantified over
    /// applications of one interaction, and no identity defines which
    /// applications those are.
    ExternalIdempotencyRequiresIdentity {
        effect: Id,
    },

    /// An external effect declares `result_replay: replay_stable`
    /// without a keyed interaction identity: the fixed terminal result
    /// is a fact about one interaction, and no identity defines it.
    ExternalReplayStabilityRequiresIdentity {
        effect: Id,
    },

    /// An external effect declares a `result_replay` behaviour —
    /// `unstable` or `replay_stable` — while declaring no result
    /// contract: there is no modeled synchronous result whose replay
    /// behaviour could be described.
    ExternalResultReplayWithoutResult {
        effect: Id,
    },

    /// A `join_all` declares no handles; the barrier would wait on
    /// nothing.
    EmptyJoinAll {
        operation: Id,
        location: StepLocation,
    },

    /// A `race` declares fewer than two handles; a first-completion
    /// barrier over one candidate is a `join_all`.
    RaceRequiresTwoHandles {
        operation: Id,
        location: StepLocation,
        count: usize,
    },

    /// One synchronization step references the same async handle more
    /// than once.
    DuplicateSynchronizationHandle {
        operation: Id,
        location: StepLocation,
        handle: Id,
        consumer: ProgramUse,
    },

    /// A synchronization step waits on an async handle that is not
    /// definitely bound by an async launch on every path reaching it.
    AsyncHandleNotAvailable {
        operation: Id,
        location: StepLocation,
        handle: Id,
        consumer: ProgramUse,
    },

    /// A result-binding `race` whose candidates do not all expose the
    /// same logical result contract.
    RaceResultContractMismatch {
        operation: Id,
        location: StepLocation,
        bind: Id,
        first: Id,
        second: Id,
    },

    /// An `execute_effect_async` launches an effect whose kind does
    /// not permit direct asynchronous execution.
    ///
    /// Every kind currently legal for direct execution is
    /// async-capable, so no present model raises this; it exists so a
    /// future effect kind must declare its answer rather than inherit
    /// one.
    EffectKindNotAsyncCapable {
        operation: Id,
        location: StepLocation,
        effect: Id,
    },

    /// An outbox has no consuming outbox input. Exactly one
    /// `OutboxInput` must reference each outbox: consumption is
    /// intrinsic to the abstraction, so an unconsumed outbox would
    /// hold committed messages durably pending forever.
    OutboxWithoutConsumer {
        outbox: Id,
    },

    /// More than one outbox input references the same outbox. The one
    /// consuming input's operation is the outbox's exclusive logical
    /// consumer; downstream fan-out belongs to topics, not outboxes.
    OutboxMultipleConsumers {
        outbox: Id,
        first_operation: Id,
        first_input: Id,
        operation: Id,
        input: Id,
    },

    /// An outbox write declares a schema the destination outbox does
    /// not admit.
    OutboxWriteMessageNotAdmitted {
        effect: Id,
        outbox: Id,
        schema: Id,
    },

    /// An outbox message-identity mapping names a schema the outbox
    /// does not admit.
    OutboxMessageIdentitySchemaNotAdmitted {
        outbox: Id,
        schema: Id,
    },

    /// An outbox message-identity mapping declares an empty identity
    /// tuple.
    EmptyOutboxMessageIdentity {
        outbox: Id,
        schema: Id,
    },

    /// Outbox message-identity tuple positions correspond across
    /// schemas, so every mapped tuple must have the same arity.
    OutboxMessageIdentityArityMismatch {
        outbox: Id,
        schema: Id,
        expected: usize,
        actual: usize,
    },

    /// A `write_outbox` step targets an outbox owned by a data model
    /// other than the transaction's. Conseqa never infers a
    /// distributed cross-data-model atomic transaction.
    OutboxOutsideDataModel {
        transaction: Id,
        effect: Id,
        data_model: Id,
        outbox: Id,
    },

    /// A `write_outbox` step appears in a transaction that declares no
    /// data model, so no owning atomic boundary exists for the
    /// admission.
    OutboxWriteMissingDataModel {
        transaction: Id,
        effect: Id,
        outbox: Id,
    },

    /// An `OutboxWriteEffect` appears at a direct execution site.
    /// Its only legal execution site is a transaction's
    /// `write_outbox` step.
    OutboxWriteOutsideTransaction {
        operation: Id,
        effect: Id,
    },

    /// An `OutboxWriteEffect` appears as an effect-intent contract.
    /// An outbox message is durable typed application data admitted
    /// with a commit, not a captured effect instance for later
    /// execution.
    OutboxWriteCannotBeIntent {
        transaction: Id,
        effect: Id,
    },

    /// An outbox partition mapping names a schema the outbox does not
    /// admit.
    OutboxPartitionSchemaNotAdmitted {
        input: Id,
        outbox: Id,
        schema: Id,
    },

    /// A keyed outbox partitioning leaves a schema admitted through
    /// the target input unmapped, so such messages would belong to no
    /// partition.
    OutboxPartitionMissingSchema {
        input: Id,
        outbox: Id,
        schema: Id,
    },

    /// An outbox partition mapping maps a schema to an empty tuple.
    EmptyOutboxPartitionKey {
        input: Id,
        schema: Id,
    },

    /// Outbox partition-key tuple positions correspond across schemas,
    /// so every mapped tuple must have the same arity.
    OutboxPartitionKeyArityMismatch {
        input: Id,
        schema: Id,
        expected: usize,
        actual: usize,
    },

    /// `ordering: partition` is declared with `partitioning: none`, so
    /// no domain exists for the guarantee to be interpreted over.
    PartitionOrderingWithoutPartitioning {
        input: Id,
    },

    /// Outbox dispatch routes by `partition_key` while the runtime
    /// declares `partitioning: none`, so no partition-key domain
    /// exists to route. Routing consumes an already-declared semantic
    /// key rather than inventing one.
    OutboxRoutingWithoutPartitioning {
        operation: Id,
        input: Id,
        outbox: Id,
    },
}

/// Where a program point consumes a value, for a diagnostic to name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramUse {
    /// The body or commit key of a transaction executed at the step.
    Transaction { transaction: Id },

    /// The instance derivation or the declaration of an effect executed
    /// at the step.
    Effect { effect: Id },

    /// The execution of an effect intent at the step.
    EffectIntent { intent: Id },

    /// The `join_all` barrier at the step.
    JoinAll,

    /// The `race` barrier at the step.
    Race,

    /// The `match_result` at the step.
    Match,

    /// The branch condition at the step.
    Condition,

    /// The outcome returned at the step.
    Return { request: Id },
}

impl std::fmt::Display for ProgramUse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transaction { transaction } => write!(f, "transaction `{transaction}`"),
            Self::Effect { effect } => write!(f, "the execution of effect `{effect}`"),
            Self::EffectIntent { intent } => write!(f, "the execution of intent `{intent}`"),
            Self::JoinAll => f.write_str("the `join_all` barrier"),
            Self::Race => f.write_str("the `race` barrier"),
            Self::Match => f.write_str("the result match"),
            Self::Condition => f.write_str("the branch condition"),
            Self::Return { request } => write!(f, "the result returned for `{request}`"),
        }
    }
}

impl From<ValidationError> for Diagnostic {
    fn from(error: ValidationError) -> Self {
        match error {
            ValidationError::DuplicateId {
                id,
                first,
                second,
            } => {
                let first_subject =
                    first.owner.clone().or_else(|| Some(id.clone()));

                let second_subject =
                    second.owner.clone().or_else(|| Some(id.clone()));

                Diagnostic {
                    code: DiagnosticCode::Validation(
                        ValidationCode::DuplicateId,
                    ),
                    severity: Severity::Error,
                    subject: Some(id.clone()),
                    message: format!(
                        "ID `{id}` is declared more than once in the global model namespace."
                    ),
                    evidence: vec![
                        Evidence {
                            subject: first_subject,
                            message: first.describe(),
                        },
                        Evidence {
                            subject: second_subject,
                            message: second.describe(),
                        },
                    ],
                }
            }

            ValidationError::UnknownReference {
                subject,
                reference,
                expected,
            } => {
                let message = format!(
                    "`{subject}` references unknown {expected} `{reference}`."
                );

                Diagnostic {
                    code: DiagnosticCode::Validation(
                        ValidationCode::UnknownReference,
                    ),
                    severity: Severity::Error,
                    subject: Some(subject),
                    message,
                    evidence: vec![Evidence {
                        subject: Some(reference),
                        message: format!(
                            "Expected this ID to resolve to a {expected}."
                        ),
                    }],
                }
            }

            ValidationError::InvalidReferenceKind {
                subject,
                reference,
                expected,
                actual,
            } => {
                let message = format!(
                    "`{subject}` references `{reference}` as a {expected}, \
                     but it is a {actual}."
                );

                Diagnostic {
                    code: DiagnosticCode::Validation(
                        ValidationCode::InvalidReferenceKind,
                    ),
                    severity: Severity::Error,
                    subject: Some(subject),
                    message,
                    evidence: vec![Evidence {
                        subject: Some(reference),
                        message: format!(
                            "Expected {expected}, found {actual}."
                        ),
                    }],
                }
            }

            ValidationError::InvalidReferenceOwner {
                subject,
                reference,
                expected_owner,
                actual_owner,
            } => {
                let actual_owner_description =
                    match &actual_owner {
                        Some(owner) => {
                            format!("`{owner}`")
                        }

                        None => {
                            "no owner".to_string()
                        }
                    };

                Diagnostic {
                    code: DiagnosticCode::Validation(
                        ValidationCode::InvalidReferenceOwner,
                    ),
                    severity: Severity::Error,
                    subject: Some(subject),
                    message: format!(
                        "`{reference}` is referenced through `{expected_owner}`, \
                         but is not owned by it."
                    ),
                    evidence: vec![Evidence {
                        subject: Some(reference),
                        message: format!(
                            "Expected owner `{expected_owner}`, found \
                             {actual_owner_description}."
                        ),
                    }],
                }
            }

            ValidationError::EmptyRoutingKey { router } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::EmptyRoutingKey),
                severity: Severity::Error,
                subject: Some(router.clone()),
                message: format!(
                    "`{router}` declares a routing block with an empty key, which \
                     names no routing domain."
                ),
                evidence: vec![Evidence {
                    subject: Some(router),
                    message: "A routing key must name at least one field; omit the \
                              routing block entirely to declare no member affinity."
                        .to_string(),
                }],
            },

            ValidationError::RoutingWithoutGrouping {
                operation,
                input,
                topic,
            } => Diagnostic {
                code: DiagnosticCode::Validation(
                    ValidationCode::RoutingWithoutGrouping,
                ),
                severity: Severity::Error,
                subject: Some(input.clone()),
                message: format!(
                    "`{input}` of `{operation}` dispatches by `grouping_key`, but no \
                     keyed grouping is in effect for `{topic}`."
                ),
                evidence: vec![Evidence {
                    subject: Some(topic),
                    message: "Declare a keyed `grouping` — on this topic's runtime, or \
                              on this subscription if the topic declares no transport \
                              semantics — or omit the routing block."
                        .to_string(),
                }],
            },

            ValidationError::GroupingKeySchemaNotOnTopic {
                subject,
                topic,
                schema,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::GroupingKeySchemaNotOnTopic),
                severity: Severity::Error,
                subject: Some(subject),
                message: format!(
                    "The grouping key maps `{schema}`, which `{topic}` does not carry."
                ),
                evidence: vec![Evidence {
                    subject: Some(topic),
                    message: "A grouping key may only map schemas the topic carries."
                        .to_string(),
                }],
            },

            ValidationError::GroupingKeyMissingSchema {
                subject,
                topic,
                schema,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::GroupingKeyMissingSchema),
                severity: Severity::Error,
                subject: Some(subject),
                message: format!(
                    "The grouping key leaves `{schema}`, carried by `{topic}`, unmapped."
                ),
                evidence: vec![Evidence {
                    subject: Some(schema),
                    message: "A grouping key must place every carried message in some \
                              group; an unmapped schema would belong to none."
                        .to_string(),
                }],
            },

            ValidationError::EmptyGroupingKey { subject, schema } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::EmptyGroupingKey),
                severity: Severity::Error,
                subject: Some(subject),
                message: format!("The grouping key maps `{schema}` to an empty tuple."),
                evidence: vec![Evidence {
                    subject: Some(schema),
                    message: "A grouping key tuple must name at least one field."
                        .to_string(),
                }],
            },

            ValidationError::GroupingKeyArityMismatch {
                subject,
                schema,
                expected,
                actual,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::GroupingKeyArityMismatch),
                severity: Severity::Error,
                subject: Some(subject),
                message: format!(
                    "The grouping key maps `{schema}` to {actual} field(s), but other \
                     schemas map {expected}."
                ),
                evidence: vec![Evidence {
                    subject: Some(schema),
                    message: "Tuple positions correspond across schemas, so every \
                              mapped tuple shares one arity."
                        .to_string(),
                }],
            },

            ValidationError::WithinGroupWithoutGrouping { subject } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::WithinGroupWithoutGrouping),
                severity: Severity::Error,
                subject: Some(subject.clone()),
                message: "`ordering: within_group` is declared where no keyed grouping \
                          is declared at the same scope."
                    .to_string(),
                evidence: vec![Evidence {
                    subject: Some(subject),
                    message: "Declare the grouping the guarantee is about, or use \
                              `ordering: global` or `none`."
                        .to_string(),
                }],
            },

            ValidationError::TransportSemanticsAtBothScopes {
                topic,
                operation,
                input,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::TransportSemanticsAtBothScopes),
                severity: Severity::Error,
                subject: Some(input.clone()),
                message: format!(
                    "`{topic}` declares transport semantics for all its subscriptions, \
                     and `{input}` of `{operation}` declares its own."
                ),
                evidence: vec![Evidence {
                    subject: Some(topic),
                    message: "Grouping and ordering are declared either once for the \
                              topic or independently per subscription, never at both. \
                              There is no override."
                        .to_string(),
                }],
            },

            ValidationError::DuplicateRouterForBoundary {
                first,
                second,
                operation,
                input,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::DuplicateRouterForBoundary),
                severity: Severity::Error,
                subject: Some(second.clone()),
                message: format!(
                    "`{first}` and `{second}` both route the request boundary \
                     `{operation}`/`{input}`."
                ),
                evidence: vec![Evidence {
                    subject: Some(first),
                    message: "One request boundary has at most one router, so its \
                              execution-pool assignment is unambiguous."
                        .to_string(),
                }],
            },

            ValidationError::EmptyPartitionKey { layout } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::EmptyPartitionKey),
                severity: Severity::Error,
                subject: Some(layout.clone()),
                message: format!(
                    "`{layout}` declares an empty partition key, which identifies no \
                     physical partition."
                ),
                evidence: vec![Evidence {
                    subject: Some(layout),
                    message: "A partition key must name at least one field of the \
                              object's schema."
                        .to_string(),
                }],
            },

            ValidationError::DuplicateStorageLayoutForObject {
                first,
                second,
                data_model,
                object,
            } => Diagnostic {
                code: DiagnosticCode::Validation(
                    ValidationCode::DuplicateStorageLayoutForObject,
                ),
                severity: Severity::Error,
                subject: Some(second.clone()),
                message: format!(
                    "`{first}` and `{second}` both declare a storage layout for \
                     `{data_model}`/`{object}`."
                ),
                evidence: vec![Evidence {
                    subject: Some(first),
                    message: "V1 admits at most one primary storage layout per data \
                              object."
                        .to_string(),
                }],
            },

            ValidationError::InvalidFieldPath {
                subject,
                schema,
                path,
            } => {
                let message = format!(
                    "Field path `{path}` does not resolve against schema `{schema}`."
                );

                Diagnostic {
                    code: DiagnosticCode::Validation(
                        ValidationCode::InvalidFieldPath,
                    ),
                    severity: Severity::Error,
                    subject: Some(subject),
                    message,
                    evidence: vec![Evidence {
                        subject: Some(schema),
                        message: format!(
                            "Could not resolve field path `{path}` from this schema."
                        ),
                    }],
                }
            }

            ValidationError::ValueSourceHasNoSchema {
                subject,
                source,
            } => {
                Diagnostic {
                    code: DiagnosticCode::Validation(
                        ValidationCode::ValueSourceHasNoSchema,
                    ),
                    severity: Severity::Error,
                    subject: Some(subject.clone()),
                    message: format!(
                        "`{subject}` references fields of `{source}`, \
                         but that value source has no modeled payload schema."
                    ),
                    evidence: vec![Evidence {
                        subject: Some(source),
                        message:
                            "A field path can only be resolved from a structurally typed value source."
                                .to_string(),
                    }],
                }
            }

            ValidationError::ValueSourceOutOfScope {
                subject,
                source,
                owner,
            } => {
                Diagnostic {
                    code: DiagnosticCode::Validation(
                        ValidationCode::ValueSourceOutOfScope,
                    ),
                    severity: Severity::Error,
                    subject: Some(subject.clone()),
                    message: format!(
                        "`{subject}` references `{source}`, which belongs to \
                         `{owner}` and is not observable by the invocations \
                         that evaluate this reference."
                    ),
                    evidence: vec![Evidence {
                        subject: Some(owner),
                        message:
                            "A value reference may only name inputs, effects, transaction outputs, and effect results reachable from the operation whose invocation evaluates it."
                                .to_string(),
                    }],
                }
            }

            ValidationError::FragmentCycle {
                cycle,
            } => {
                let rendered_cycle = cycle
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" -> ");

                let subject = cycle.first().cloned();

                Diagnostic {
                    code: DiagnosticCode::Validation(
                        ValidationCode::FragmentCycle,
                    ),
                    severity: Severity::Error,
                    subject,
                    message: format!(
                        "Schema fragment derivation contains a cycle: \
                         {rendered_cycle}."
                    ),
                    evidence: vec![],
                }
            }

            ValidationError::DataObjectSchemaNotCanonical {
                object,
                schema,
            } => {
                let message = format!(
                    "Data object `{object}` references non-canonical \
                     schema `{schema}`."
                );

                Diagnostic {
                    code: DiagnosticCode::Validation(
                        ValidationCode::DataObjectSchemaNotCanonical,
                    ),
                    severity: Severity::Error,
                    subject: Some(object),
                    message,
                    evidence: vec![Evidence {
                        subject: Some(schema),
                        message:
                            "A data object's state must be described by a canonical schema."
                                .to_string(),
                    }],
                }
            }

            ValidationError::SubscriptionMessageNotOnTopic {
                input,
                topic,
                schema,
            } => {
                let message = format!(
                    "Subscription input `{input}` selects schema `{schema}`, \
                     which is not carried by topic `{topic}`."
                );

                Diagnostic {
                    code: DiagnosticCode::Validation(
                        ValidationCode::SubscriptionMessageNotOnTopic,
                    ),
                    severity: Severity::Error,
                    subject: Some(input),
                    message,
                    evidence: vec![Evidence {
                        subject: Some(topic),
                        message: format!(
                            "Topic does not declare schema `{schema}` as a message."
                        ),
                    }],
                }
            }

            ValidationError::PublicationEffectMessageNotOnTopic {
                effect,
                topic,
                schema,
            } => {
                let message = format!(
                    "Publication effect `{effect}` publishes schema `{schema}` \
                     to topic `{topic}`, but the topic does not carry that schema."
                );

                Diagnostic {
                    code: DiagnosticCode::Validation(
                        ValidationCode::PublicationEffectMessageNotOnTopic,
                    ),
                    severity: Severity::Error,
                    subject: Some(effect),
                    message,
                    evidence: vec![Evidence {
                        subject: Some(topic),
                        message: format!(
                            "Topic does not declare schema `{schema}` as a message."
                        ),
                    }],
                }
            }

            ValidationError::MessageIdentitySchemaNotOnTopic {
                topic,
                schema,
            } => {
                Diagnostic {
                    code: DiagnosticCode::Validation(
                        ValidationCode::MessageIdentitySchemaNotOnTopic,
                    ),
                    severity: Severity::Error,
                    subject: Some(topic.clone()),
                    message: format!(
                        "Topic `{topic}` declares a message identity for schema \
                         `{schema}`, but does not carry that schema."
                    ),
                    evidence: vec![Evidence {
                        subject: Some(schema),
                        message:
                            "Message-identity mappings may only reference message schemas carried by the topic."
                                .to_string(),
                    }],
                }
            }

            ValidationError::EmptyMessageIdentity {
                topic,
                schema,
            } => {
                Diagnostic {
                    code: DiagnosticCode::Validation(
                        ValidationCode::EmptyMessageIdentity,
                    ),
                    severity: Severity::Error,
                    subject: Some(topic.clone()),
                    message: format!(
                        "Topic `{topic}` declares an empty message identity \
                         for schema `{schema}`."
                    ),
                    evidence: vec![Evidence {
                        subject: Some(schema),
                        message:
                            "A mapped schema must declare the complete, non-empty identity of one logical message."
                                .to_string(),
                    }],
                }
            }

            ValidationError::MessageIdentityArityMismatch {
                topic,
                schema,
                expected,
                actual,
            } => {
                Diagnostic {
                    code: DiagnosticCode::Validation(
                        ValidationCode::MessageIdentityArityMismatch,
                    ),
                    severity: Severity::Error,
                    subject: Some(topic.clone()),
                    message: format!(
                        "Topic `{topic}` maps the message identity of \
                         `{schema}` with {actual} field(s), but other mapped \
                         schemas use {expected}."
                    ),
                    evidence: vec![Evidence {
                        subject: Some(schema),
                        message:
                            "Identity tuple positions correspond across schemas, so every mapped tuple must have the same arity."
                                .to_string(),
                    }],
                }
            }

            ValidationError::EmptyRequestIdentity {
                input,
            } => {
                Diagnostic {
                    code: DiagnosticCode::Validation(
                        ValidationCode::EmptyRequestIdentity,
                    ),
                    severity: Severity::Error,
                    subject: Some(input.clone()),
                    message: format!(
                        "Request input `{input}` declares a keyed identity \
                         with no fields."
                    ),
                    evidence: vec![Evidence {
                        subject: Some(input),
                        message:
                            "A keyed request identity must declare the complete, non-empty identity of one logical request."
                                .to_string(),
                    }],
                }
            }

            ValidationError::TransactionRequirementKeyUnavailable {
                operation,
                location,
                transaction,
                family,
                requirement,
                key,
                reason,
            } => Diagnostic {
                code: DiagnosticCode::Validation(
                    ValidationCode::TransactionRequirementKeyUnavailable,
                ),
                severity: Severity::Error,
                subject: Some(transaction.clone()),
                message: format!(
                    "The key of {family} requirement {requirement} of `{transaction}` \
                     (step `{location}` of `{operation}`), `{}.{}`, is not available when \
                     the transaction begins: {reason}.",
                    key.source.id(),
                    key.path
                ),
                evidence: vec![Evidence {
                    subject: Some(transaction),
                    message: "A transaction requirement key identifies the conflict domain \
                              before the transaction executes, so it may derive only from an \
                              input, a prior transaction output, or a synchronous result \
                              already bound on every reaching path — never from a read the \
                              transaction itself performs."
                        .to_string(),
                }],
            },

            ValidationError::TransactionOrderingPositionUnavailable {
                operation,
                location,
                transaction,
                requirement,
                position,
                reason,
            } => Diagnostic {
                code: DiagnosticCode::Validation(
                    ValidationCode::TransactionOrderingPositionUnavailable,
                ),
                severity: Severity::Error,
                subject: Some(transaction.clone()),
                message: format!(
                    "The position of ordering requirement {requirement} of `{transaction}` \
                     (step `{location}` of `{operation}`), `{}.{}`, is not available when \
                     the transaction begins: {reason}.",
                    position.source.id(),
                    position.path
                ),
                evidence: vec![Evidence {
                    subject: Some(transaction),
                    message: "An ordering position is the transaction's logical precedence \
                              within its domain, fixed before it executes: an input, a prior \
                              transaction output, or a synchronous result already bound on \
                              every reaching path."
                        .to_string(),
                }],
            },

            ValidationError::TransactionOrderingPositionNotOrderedScalar {
                operation,
                transaction,
                requirement,
                position,
                found,
            } => Diagnostic {
                code: DiagnosticCode::Validation(
                    ValidationCode::TransactionOrderingPositionNotOrderedScalar,
                ),
                severity: Severity::Error,
                subject: Some(transaction.clone()),
                message: format!(
                    "The position of ordering requirement {requirement} of `{transaction}` \
                     in `{operation}`, `{}.{}`, is {found}; an ordering position must be a \
                     non-optional int, decimal, or timestamp.",
                    position.source.id(),
                    position.path
                ),
                evidence: vec![Evidence {
                    subject: Some(transaction),
                    message: "`float` is excluded because NaN and implementation-specific \
                              comparison make it no total order; `uuid`, `bool`, structured \
                              schemas, and lists are not positions."
                        .to_string(),
                }],
            },

            ValidationError::MissingTransactionRejectedArm {
                operation,
                location,
                transaction,
                step,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::MissingTransactionRejectedArm),
                severity: Severity::Error,
                subject: Some(transaction.clone()),
                message: format!(
                    "Transaction `{transaction}` at step `{location}` of `{operation}` can \
                     reject — its step {} is a commit guard — but the step declares no \
                     `rejected` block.",
                    step + 1
                ),
                evidence: vec![Evidence {
                    subject: Some(transaction),
                    message: "A transition, version validation, cursor advance, or fence may \
                              conclusively fail at commit; the operation must say what \
                              control does then. Declare `rejected` with the block control \
                              enters when the transaction rejects."
                        .to_string(),
                }],
            },

            ValidationError::UnexpectedTransactionRejectedArm {
                operation,
                location,
                transaction,
            } => Diagnostic {
                code: DiagnosticCode::Validation(
                    ValidationCode::UnexpectedTransactionRejectedArm,
                ),
                severity: Severity::Error,
                subject: Some(transaction.clone()),
                message: format!(
                    "Transaction `{transaction}` at step `{location}` of `{operation}` \
                     declares a `rejected` block, but no step of its body can reject."
                ),
                evidence: vec![Evidence {
                    subject: Some(transaction),
                    message: "Only a transition, version validation, cursor advance, or \
                              fence rejects; a transaction without one either commits or is \
                              interrupted, and interruption never enters `rejected`. Remove \
                              the block."
                        .to_string(),
                }],
            },

            ValidationError::UnknownTransitionOutbox {
                machine,
                transition,
                effect,
                outbox,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::UnknownTransitionOutbox),
                severity: Severity::Error,
                subject: Some(transition.clone()),
                message: format!(
                    "Transition `{transition}` of `{machine}` admits `{effect}` to outbox \
                     `{outbox}`, which no data model declares."
                ),
                evidence: vec![Evidence {
                    subject: Some(outbox),
                    message: "A transition-scoped outbox write names an outbox of the data \
                              model that owns the machine's subject object."
                        .to_string(),
                }],
            },

            ValidationError::InvalidTransitionOutboxSchema {
                machine,
                transition,
                effect,
                outbox,
                schema,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::InvalidTransitionOutboxSchema),
                severity: Severity::Error,
                subject: Some(transition.clone()),
                message: format!(
                    "Transition `{transition}` of `{machine}` admits schema `{schema}` to \
                     outbox `{outbox}` through `{effect}`, but that schema is not one the \
                     outbox admits."
                ),
                evidence: vec![Evidence {
                    subject: Some(schema),
                    message: "The admitted schema must be declared and listed among the \
                              outbox's messages."
                        .to_string(),
                }],
            },

            ValidationError::TransitionOutboxOutsideDataModel {
                machine,
                transition,
                effect,
                outbox,
                data_model,
            } => Diagnostic {
                code: DiagnosticCode::Validation(
                    ValidationCode::TransitionOutboxOutsideDataModel,
                ),
                severity: Severity::Error,
                subject: Some(transition.clone()),
                message: format!(
                    "Transition `{transition}` of `{machine}` admits `{effect}` to outbox \
                     `{outbox}`, which does not belong to `{data_model}`, the data model \
                     that owns the machine's subject."
                ),
                evidence: vec![Evidence {
                    subject: Some(outbox),
                    message: "A transition-scoped admission is atomic with the transaction \
                              that applies the transition, and that transaction's atomic \
                              boundary is the subject's data model; Conseqa never infers a \
                              distributed cross-data-model atomic commit."
                        .to_string(),
                }],
            },

            ValidationError::InvalidTransitionOutboxDerivation {
                transaction,
                transition,
                missing,
                unexpected,
            } => {
                let mut evidence = Vec::new();

                for effect in &missing {
                    evidence.push(Evidence {
                        subject: Some(effect.clone()),
                        message: format!(
                            "The transition declares outbox effect `{effect}`, but the step \
                             provides no message derivation for it."
                        ),
                    });
                }

                for effect in &unexpected {
                    evidence.push(Evidence {
                        subject: Some(effect.clone()),
                        message: format!(
                            "The step provides a derivation for `{effect}`, which is not an \
                             outbox effect declared by transition `{transition}`."
                        ),
                    });
                }

                Diagnostic {
                    code: DiagnosticCode::Validation(
                        ValidationCode::InvalidTransitionOutboxDerivation,
                    ),
                    severity: Severity::Error,
                    subject: Some(transaction.clone()),
                    message: format!(
                        "Transaction `{transaction}` applies transition `{transition}` with \
                         `effects` that do not exactly match the transition's declared \
                         outbox effects."
                    ),
                    evidence,
                }
            }

            ValidationError::InvalidObjectVersionField {
                object,
                field,
                defect,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::InvalidObjectVersionField),
                severity: Severity::Error,
                subject: Some(object.clone()),
                message: format!(
                    "Data object `{object}` declares `{field}` as its version field, but \
                     {defect}."
                ),
                evidence: vec![Evidence {
                    subject: Some(object),
                    message: "A version field is a non-optional int outside the object's \
                              identity, managed by the version protocol alone."
                        .to_string(),
                }],
            },

            ValidationError::DirectWriteToVersionField {
                transaction,
                step,
                object,
                field,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::DirectWriteToVersionField),
                severity: Severity::Error,
                subject: Some(transaction.clone()),
                message: format!(
                    "Step {} of transaction `{transaction}` writes `{field}` of `{object}`, \
                     the object's version field.",
                    step + 1
                ),
                evidence: vec![Evidence {
                    subject: Some(object),
                    message: "A version is never assigned through a derivation: `insert` \
                              creates the initial version and `bump_version` advances it."
                        .to_string(),
                }],
            },

            ValidationError::MissingVersionBump {
                transaction,
                step,
                object,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::MissingVersionBump),
                severity: Severity::Error,
                subject: Some(transaction.clone()),
                message: format!(
                    "Step {} of transaction `{transaction}` mutates versioned object \
                     `{object}` without a `bump_version` of the same selected instance.",
                    step + 1
                ),
                evidence: vec![Evidence {
                    subject: Some(object),
                    message: "Every write or transition of a live versioned instance must be \
                              accompanied by a `bump_version` whose selector is exactly the \
                              mutation's, so a validating reader detects the mutation at \
                              commit."
                        .to_string(),
                }],
            },

            ValidationError::DuplicateVersionBump {
                transaction,
                step,
                object,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::DuplicateVersionBump),
                severity: Severity::Error,
                subject: Some(transaction.clone()),
                message: format!(
                    "Step {} of transaction `{transaction}` bumps the version of an \
                     `{object}` instance an earlier step already bumps.",
                    step + 1
                ),
                evidence: vec![Evidence {
                    subject: Some(object),
                    message: "One transaction bumps one selected instance at most once: the \
                              version advances by exactly one per successful commit."
                        .to_string(),
                }],
            },

            ValidationError::VersionValidationWithoutObservedVersion {
                transaction,
                step,
                object,
            } => Diagnostic {
                code: DiagnosticCode::Validation(
                    ValidationCode::VersionValidationWithoutObservedVersion,
                ),
                severity: Severity::Error,
                subject: Some(transaction.clone()),
                message: format!(
                    "Step {} of transaction `{transaction}` validates the version of \
                     `{object}` against a value that is not a preceding read of that \
                     instance's version field.",
                    step + 1
                ),
                evidence: vec![Evidence {
                    subject: Some(object),
                    message: "`expected` must be `transaction_read:<bind>.<version field>` of \
                              an earlier read that selects the same instance and covers the \
                              version field; only an observed version makes the validation a \
                              commit guard."
                        .to_string(),
                }],
            },

            ValidationError::VersionProtocolOnUnversionedObject {
                transaction,
                step,
                object,
            } => Diagnostic {
                code: DiagnosticCode::Validation(
                    ValidationCode::VersionProtocolOnUnversionedObject,
                ),
                severity: Severity::Error,
                subject: Some(transaction.clone()),
                message: format!(
                    "Step {} of transaction `{transaction}` applies the version protocol to \
                     `{object}`, which declares no version.",
                    step + 1
                ),
                evidence: vec![Evidence {
                    subject: Some(object),
                    message: "Declare `version` on the data object before validating or \
                              bumping it."
                        .to_string(),
                }],
            },

            ValidationError::ManagedFieldRoleConflict {
                object,
                field,
                first,
                second,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::ManagedFieldRoleConflict),
                severity: Severity::Error,
                subject: Some(object.clone()),
                message: format!(
                    "Field `{field}` of `{object}` is used as a {first} and as a {second}."
                ),
                evidence: vec![Evidence {
                    subject: Some(object),
                    message: "A managed field has exactly one semantic role: a version, a \
                              cursor under one rule, or a fence."
                        .to_string(),
                }],
            },

            ValidationError::DirectWriteToManagedField {
                transaction,
                step,
                object,
                field,
                role,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::DirectWriteToManagedField),
                severity: Severity::Error,
                subject: Some(transaction.clone()),
                message: format!(
                    "Step {} of transaction `{transaction}` writes `{field}` of `{object}`, \
                     a managed {role} field.",
                    step + 1
                ),
                evidence: vec![Evidence {
                    subject: Some(object),
                    message: "A cursor advances only through `advance_cursor` and a fence \
                              only through `fence`; an ordinary write would break the order \
                              their proofs rest on. Insert initialization remains permitted."
                        .to_string(),
                }],
            },

            ValidationError::InvalidManagedFieldType {
                object,
                field,
                role,
                defect,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::InvalidManagedFieldType),
                severity: Severity::Error,
                subject: Some(object.clone()),
                message: format!(
                    "Field `{field}` of `{object}` is used as a {role}, but {defect}."
                ),
                evidence: vec![Evidence {
                    subject: Some(object),
                    message: "A successor cursor is a non-optional int; a monotonic cursor or \
                              a fence is a non-optional int, decimal, or timestamp; and the \
                              value driving it has the field's type."
                        .to_string(),
                }],
            },

            ValidationError::UnknownResultErrorClass {
                operation,
                location,
                request,
                error,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::UnknownResultErrorClass),
                severity: Severity::Error,
                subject: Some(operation.clone()),
                message: format!(
                    "Program step `{location}` of `{operation}` returns error class \
                     `{error}` for `{request}`, which declares no such class."
                ),
                evidence: vec![Evidence {
                    subject: Some(request),
                    message: "A returned error names one of the classes in the request's \
                              result contract; its derivation must match that class's \
                              schema."
                        .to_string(),
                }],
            },

            ValidationError::MissingResultErrorArm {
                operation,
                location,
                result,
                error,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::MissingResultErrorArm),
                severity: Severity::Error,
                subject: Some(operation.clone()),
                message: format!(
                    "Program step `{location}` of `{operation}` matches `{result}` without \
                     an arm for its error class `{error}`."
                ),
                evidence: vec![Evidence {
                    subject: Some(result),
                    message: "Error arms are explicit and exhaustive over the result \
                              contract's error classes."
                        .to_string(),
                }],
            },

            ValidationError::UnexpectedResultErrorArm {
                operation,
                location,
                result,
                error,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::UnexpectedResultErrorArm),
                severity: Severity::Error,
                subject: Some(operation.clone()),
                message: format!(
                    "Program step `{location}` of `{operation}` matches `{result}` with an \
                     arm for `{error}`, which is not an error class of its contract."
                ),
                evidence: vec![Evidence {
                    subject: Some(result),
                    message: "Only the classes the result contract declares have arms."
                        .to_string(),
                }],
            },

            ValidationError::TransactionObjectOutsideDataModel {
                transaction,
                data_model,
                object,
            } => {
                let message = format!(
                    "Transaction `{transaction}` accesses object `{object}`, \
                     which does not belong to its declared data model `{data_model}`."
                );

                Diagnostic {
                    code: DiagnosticCode::Validation(
                        ValidationCode::TransactionObjectOutsideDataModel,
                    ),
                    severity: Severity::Error,
                    subject: Some(transaction),
                    message,
                    evidence: vec![Evidence {
                        subject: Some(object),
                        message: format!(
                            "This object is outside data model `{data_model}`."
                        ),
                    }],
                }
            }

            ValidationError::TransactionMissingDataModel {
                transaction,
                object,
            } => {
                Diagnostic {
                    code: DiagnosticCode::Validation(
                        ValidationCode::TransactionMissingDataModel,
                    ),
                    severity: Severity::Error,
                    subject: Some(transaction.clone()),
                    message: format!(
                        "Transaction `{transaction}` accesses data object \
                         `{object}` but declares no data-model boundary."
                    ),
                    evidence: vec![Evidence {
                        subject: Some(object),
                        message:
                            "Access to a persistent data object requires the transaction to declare its data model."
                                .to_string(),
                    }],
                }
            }

            ValidationError::TransactionReadOutsideTransaction {
                subject,
                read,
            } => {
                Diagnostic {
                    code: DiagnosticCode::Validation(
                        ValidationCode::TransactionReadOutsideTransaction,
                    ),
                    severity: Severity::Error,
                    subject: Some(subject.clone()),
                    message: format!(
                        "`{subject}` references transaction-read result `{read}` \
                         outside the transaction execution that produces it."
                    ),
                    evidence: vec![Evidence {
                        subject: Some(read),
                        message:
                            "A transaction-read result is local to its transaction execution and never becomes a durable cross-transaction artifact."
                                .to_string(),
                    }],
                }
            }

            ValidationError::TransactionReadOutOfOrder {
                transaction,
                read,
            } => {
                Diagnostic {
                    code: DiagnosticCode::Validation(
                        ValidationCode::TransactionReadOutOfOrder,
                    ),
                    severity: Severity::Error,
                    subject: Some(transaction.clone()),
                    message: format!(
                        "Transaction `{transaction}` references transaction-read \
                         result `{read}` before the step that reads it."
                    ),
                    evidence: vec![Evidence {
                        subject: Some(read),
                        message:
                            "A transaction-read result may only be referenced by steps that follow the read in transaction program order."
                                .to_string(),
                    }],
                }
            }

            ValidationError::TransactionReadFieldNotSelected {
                transaction,
                read,
                path,
            } => {
                Diagnostic {
                    code: DiagnosticCode::Validation(
                        ValidationCode::TransactionReadFieldNotSelected,
                    ),
                    severity: Severity::Error,
                    subject: Some(transaction),
                    message: format!(
                        "Transaction-read result `{read}` is referenced through \
                         field path `{path}`, which the read does not select."
                    ),
                    evidence: vec![Evidence {
                        subject: Some(read),
                        message: format!(
                            "This read does not include `{path}` in its field selection."
                        ),
                    }],
                }
            }

            ValidationError::StateTransitionSubjectMismatch {
                transaction,
                machine,
                expected_object,
                actual_object,
            } => {
                Diagnostic {
                    code: DiagnosticCode::Validation(
                        ValidationCode::StateTransitionSubjectMismatch,
                    ),
                    severity: Severity::Error,
                    subject: Some(transaction),
                    message: format!(
                        "State-machine transition for `{machine}` selects \
                         data object `{actual_object}`, but the state machine \
                         governs `{expected_object}`."
                    ),
                    evidence: vec![
                        Evidence {
                            subject: Some(machine),
                            message: format!(
                                "This state machine governs data object \
                                 `{expected_object}`."
                            ),
                        },
                        Evidence {
                            subject: Some(actual_object),
                            message:
                                "This data object is selected as the subject of the transaction's state transition."
                                    .to_string(),
                        },
                    ],
                }
            }

            ValidationError::TransitionEffectIntentsMismatch {
                transaction,
                transition,
                missing,
                unexpected,
            } => {
                let mut evidence = Vec::new();

                for effect in &missing {
                    evidence.push(Evidence {
                        subject: Some(effect.clone()),
                        message: format!(
                            "The transition declares side effect `{effect}`, \
                             but the step provides no intent binding and value \
                             derivation for it."
                        ),
                    });
                }

                for effect in &unexpected {
                    evidence.push(Evidence {
                        subject: Some(effect.clone()),
                        message: format!(
                            "The step provides an intent binding for \
                             `{effect}`, which is not a side effect declared \
                             by transition `{transition}`."
                        ),
                    });
                }

                Diagnostic {
                    code: DiagnosticCode::Validation(
                        ValidationCode::TransitionEffectIntentsMismatch,
                    ),
                    severity: Severity::Error,
                    subject: Some(transaction.clone()),
                    message: format!(
                        "Transaction `{transaction}` applies transition \
                         `{transition}` with `effect_intents` that do not \
                         exactly match the transition's declared side effects."
                    ),
                    evidence,
                }
            }

            ValidationError::EmptyObjectIdentity {
                object,
            } => {
                Diagnostic {
                    code: DiagnosticCode::Validation(
                        ValidationCode::EmptyObjectIdentity,
                    ),
                    severity: Severity::Error,
                    subject: Some(object.clone()),
                    message: format!(
                        "Data object `{object}` declares an empty identity."
                    ),
                    evidence: vec![Evidence {
                        subject: Some(object),
                        message:
                            "Every data object must declare the complete, non-empty logical identity of one instance."
                                .to_string(),
                    }],
                }
            }

            ValidationError::InvalidInputKind {
                subject,
                input,
                expected,
                actual,
            } => {
                Diagnostic {
                    code: DiagnosticCode::Validation(
                        ValidationCode::InvalidInputKind,
                    ),
                    severity: Severity::Error,
                    subject: Some(subject),
                    message: format!(
                        "`{input}` is referenced as a {expected}, \
                         but it is a {actual}."
                    ),
                    evidence: vec![Evidence {
                        subject: Some(input),
                        message: format!(
                            "Expected {expected}, found {actual}."
                        ),
                    }],
                }
            }

            ValidationError::ProgramNotTerminated { operation } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::ProgramNotTerminated),
                severity: Severity::Error,
                subject: Some(operation.clone()),
                message: format!(
                    "The program of `{operation}` can fall off the end of its last \
                     step without reaching a terminal."
                ),
                evidence: vec![Evidence {
                    subject: Some(operation),
                    message: "Every reachable path through an operation program must end \
                              at an explicit `return` or `complete` step."
                        .to_string(),
                }],
            },

            ValidationError::UnreachableProgramStep {
                operation,
                location,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::UnreachableProgramStep),
                severity: Severity::Error,
                subject: Some(operation.clone()),
                message: format!(
                    "Program step `{location}` of `{operation}` is unreachable: it \
                     follows a terminal in its block."
                ),
                evidence: vec![Evidence {
                    subject: Some(operation),
                    message: "A `return` or `complete` step, or a decision whose every arm \
                              terminates, ends its block; no step may follow it there."
                        .to_string(),
                }],
            },

            ValidationError::TransactionArtifactNotAvailable {
                operation,
                location,
                artifact,
                consumer,
            } => Diagnostic {
                code: DiagnosticCode::Validation(
                    ValidationCode::TransactionArtifactNotAvailable,
                ),
                severity: Severity::Error,
                subject: Some(operation.clone()),
                message: format!(
                    "Program step `{location}` of `{operation}` consumes transaction \
                     artifact `{artifact}` in {consumer}, but no transaction on every \
                     path reaching that step establishes or recovers it."
                ),
                evidence: vec![Evidence {
                    subject: Some(artifact),
                    message: "A transaction output or effect intent may be consumed only \
                              where control flow definitely establishes it: after a \
                              transaction that establishes it on every incoming path."
                        .to_string(),
                }],
            },

            ValidationError::EffectResultNotBound {
                operation,
                location,
                result,
                consumer,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::EffectResultNotBound),
                severity: Severity::Error,
                subject: Some(operation.clone()),
                message: format!(
                    "Program step `{location}` of `{operation}` uses effect result \
                     `{result}` in {consumer}, but no effect-executing step on every \
                     path reaching it binds that result."
                ),
                evidence: vec![Evidence {
                    subject: Some(result),
                    message: "A result binding is available only after the step that binds \
                              it, on every path reaching the use."
                        .to_string(),
                }],
            },

            ValidationError::EffectResultVariantOutOfScope {
                operation,
                location,
                result,
                variant,
                consumer,
            } => Diagnostic {
                code: DiagnosticCode::Validation(
                    ValidationCode::EffectResultVariantOutOfScope,
                ),
                severity: Severity::Error,
                subject: Some(operation.clone()),
                message: format!(
                    "Program step `{location}` of `{operation}` references the `{variant}` \
                     payload of `{result}` in {consumer}, outside the `{variant}` arm of \
                     a match on that result."
                ),
                evidence: vec![Evidence {
                    subject: Some(result),
                    message: "A variant payload is arm-local: `effect_result_ok` is available \
                              only inside the `ok` arm of a `match_result` on the binding, \
                              and `effect_result_err` only inside the `err` arm."
                        .to_string(),
                }],
            },

            ValidationError::EffectHasNoResult {
                operation,
                location,
                effect,
                result,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::EffectHasNoResult),
                severity: Severity::Error,
                subject: Some(operation.clone()),
                message: format!(
                    "Program step `{location}` of `{operation}` binds result `{result}` of \
                     effect `{effect}`, whose contract has no synchronous result."
                ),
                evidence: vec![Evidence {
                    subject: Some(effect),
                    message: "A publication produces no synchronous result, and an external \
                              effect produces one only when it declares a `result` contract; \
                              a request inherits its target input's contract."
                        .to_string(),
                }],
            },

            ValidationError::ExternalIdempotencyRequiresIdentity { effect } => Diagnostic {
                code: DiagnosticCode::Validation(
                    ValidationCode::ExternalIdempotencyRequiresIdentity,
                ),
                severity: Severity::Error,
                subject: Some(effect.clone()),
                message: format!(
                    "External effect `{effect}` declares `idempotency: \
                     identical_per_identity` with `identity: unspecified`."
                ),
                evidence: vec![Evidence {
                    subject: Some(effect),
                    message: "`identical_per_identity` is quantified over applications of \
                              one logical external interaction; a keyed `identity` is what \
                              defines which applications those are. Declare `identity: \
                              keyed` with the interaction key, or use `side_effect_free` \
                              for a keyless universal guarantee."
                        .to_string(),
                }],
            },

            ValidationError::ExternalReplayStabilityRequiresIdentity { effect } => Diagnostic {
                code: DiagnosticCode::Validation(
                    ValidationCode::ExternalReplayStabilityRequiresIdentity,
                ),
                severity: Severity::Error,
                subject: Some(effect.clone()),
                message: format!(
                    "External effect `{effect}` declares `result_replay: replay_stable` \
                     with `identity: unspecified`."
                ),
                evidence: vec![Evidence {
                    subject: Some(effect),
                    message: "A replay-fixed terminal result is a fact about one logical \
                              external interaction; a keyed `identity` is what defines \
                              which applications share it. Declare `identity: keyed` with \
                              the interaction key, or leave `result_replay` at \
                              `unspecified` or `unstable`."
                        .to_string(),
                }],
            },

            ValidationError::ExternalResultReplayWithoutResult { effect } => Diagnostic {
                code: DiagnosticCode::Validation(
                    ValidationCode::ExternalResultReplayWithoutResult,
                ),
                severity: Severity::Error,
                subject: Some(effect.clone()),
                message: format!(
                    "External effect `{effect}` declares a `result_replay` behaviour but \
                     no `result` contract."
                ),
                evidence: vec![Evidence {
                    subject: Some(effect),
                    message: "`unstable` and `replay_stable` describe the replay behaviour \
                              of the boundary's synchronous result; an effect declaring \
                              `result: null` has none to describe. Declare the result \
                              contract, or leave `result_replay: unspecified`."
                        .to_string(),
                }],
            },

            ValidationError::EmptyJoinAll {
                operation,
                location,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::EmptyJoinAll),
                severity: Severity::Error,
                subject: Some(operation.clone()),
                message: format!(
                    "Program step `{location}` of `{operation}` is a `join_all` with no \
                     handles."
                ),
                evidence: vec![Evidence {
                    subject: Some(operation),
                    message: "A `join_all` must reference at least one definitely available \
                              async handle; an empty barrier waits on nothing."
                        .to_string(),
                }],
            },

            ValidationError::RaceRequiresTwoHandles {
                operation,
                location,
                count,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::RaceRequiresTwoHandles),
                severity: Severity::Error,
                subject: Some(operation.clone()),
                message: format!(
                    "Program step `{location}` of `{operation}` is a `race` over {count} \
                     handle(s); a race requires at least two candidates."
                ),
                evidence: vec![Evidence {
                    subject: Some(operation),
                    message: "A first-completion barrier over fewer than two candidates \
                              decides nothing; to await one handle, use a single-handle \
                              `join_all`."
                        .to_string(),
                }],
            },

            ValidationError::DuplicateSynchronizationHandle {
                operation,
                location,
                handle,
                consumer,
            } => Diagnostic {
                code: DiagnosticCode::Validation(
                    ValidationCode::DuplicateSynchronizationHandle,
                ),
                severity: Severity::Error,
                subject: Some(operation.clone()),
                message: format!(
                    "Program step `{location}` of `{operation}` references async handle \
                     `{handle}` more than once in {consumer}."
                ),
                evidence: vec![Evidence {
                    subject: Some(handle),
                    message: "Each handle identifies one asynchronous execution occurrence \
                              and may appear at most once per synchronization step."
                        .to_string(),
                }],
            },

            ValidationError::AsyncHandleNotAvailable {
                operation,
                location,
                handle,
                consumer,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::AsyncHandleNotAvailable),
                severity: Severity::Error,
                subject: Some(operation.clone()),
                message: format!(
                    "Program step `{location}` of `{operation}` waits on async handle \
                     `{handle}` in {consumer}, but no async launch on every path reaching \
                     that step binds it."
                ),
                evidence: vec![Evidence {
                    subject: Some(handle),
                    message: "An async handle follows the operation-local definite-availability \
                              discipline: it may be synchronized only after the launch that \
                              binds it, on every path reaching the synchronization."
                        .to_string(),
                }],
            },

            ValidationError::RaceResultContractMismatch {
                operation,
                location,
                bind,
                first,
                second,
            } => Diagnostic {
                code: DiagnosticCode::Validation(
                    ValidationCode::RaceResultContractMismatch,
                ),
                severity: Severity::Error,
                subject: Some(operation.clone()),
                message: format!(
                    "Program step `{location}` of `{operation}` binds race result `{bind}`, \
                     but candidate effects `{first}` and `{second}` do not expose the same \
                     logical result contract."
                ),
                evidence: vec![Evidence {
                    subject: Some(bind),
                    message: "A result-binding race observes whichever candidate completes \
                              first, so every candidate must be result-bearing with one \
                              logical result contract; drop `bind` to race heterogeneous \
                              effects."
                        .to_string(),
                }],
            },

            ValidationError::EffectKindNotAsyncCapable {
                operation,
                location,
                effect,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::EffectKindNotAsyncCapable),
                severity: Severity::Error,
                subject: Some(operation.clone()),
                message: format!(
                    "Program step `{location}` of `{operation}` launches effect `{effect}` \
                     asynchronously, but its effect kind does not permit direct \
                     asynchronous execution."
                ),
                evidence: vec![Evidence {
                    subject: Some(effect),
                    message: "Each effect kind explicitly declares whether \
                              `execute_effect_async` may launch it; none becomes \
                              async-capable merely by being an effect."
                        .to_string(),
                }],
            },

            ValidationError::OutboxWithoutConsumer { outbox } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::OutboxWithoutConsumer),
                severity: Severity::Error,
                subject: Some(outbox.clone()),
                message: format!(
                    "Outbox `{outbox}` has no consuming outbox input; exactly one \
                     `OutboxInput` must reference it."
                ),
                evidence: vec![Evidence {
                    subject: Some(outbox),
                    message: "Consumption is intrinsic to the outbox abstraction: a \
                              committed message stays durably pending until \
                              successfully consumed, so an outbox without its one \
                              consumer would accumulate pending messages forever."
                        .to_string(),
                }],
            },

            ValidationError::OutboxMultipleConsumers {
                outbox,
                first_operation,
                first_input,
                operation,
                input,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::OutboxMultipleConsumers),
                severity: Severity::Error,
                subject: Some(input.clone()),
                message: format!(
                    "Outbox input `{input}` of `{operation}` references outbox \
                     `{outbox}`, which `{first_input}` of `{first_operation}` \
                     already consumes; exactly one `OutboxInput` may reference an \
                     outbox."
                ),
                evidence: vec![Evidence {
                    subject: Some(outbox),
                    message: "The consuming input's operation is the outbox's \
                              exclusive logical consumer of every admitted schema. \
                              Downstream fan-out belongs to topics, not outboxes: \
                              relay the messages onto a topic and subscribe the \
                              other consumers there."
                        .to_string(),
                }],
            },

            ValidationError::OutboxWriteMessageNotAdmitted {
                effect,
                outbox,
                schema,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::OutboxWriteMessageNotAdmitted),
                severity: Severity::Error,
                subject: Some(effect.clone()),
                message: format!(
                    "Outbox write `{effect}` admits schema `{schema}` to outbox \
                     `{outbox}`, but the outbox does not admit that schema."
                ),
                evidence: vec![Evidence {
                    subject: Some(outbox),
                    message: format!(
                        "The outbox does not declare schema `{schema}` as a message."
                    ),
                }],
            },

            ValidationError::OutboxMessageIdentitySchemaNotAdmitted { outbox, schema } => {
                Diagnostic {
                    code: DiagnosticCode::Validation(
                        ValidationCode::OutboxMessageIdentitySchemaNotAdmitted,
                    ),
                    severity: Severity::Error,
                    subject: Some(outbox.clone()),
                    message: format!(
                        "Outbox `{outbox}` declares a message identity for schema \
                         `{schema}`, but does not admit that schema."
                    ),
                    evidence: vec![Evidence {
                        subject: Some(schema),
                        message: "Message-identity mappings may only reference message \
                                  schemas the outbox admits."
                            .to_string(),
                    }],
                }
            }

            ValidationError::EmptyOutboxMessageIdentity { outbox, schema } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::EmptyOutboxMessageIdentity),
                severity: Severity::Error,
                subject: Some(outbox.clone()),
                message: format!(
                    "Outbox `{outbox}` declares an empty message identity for \
                     schema `{schema}`."
                ),
                evidence: vec![Evidence {
                    subject: Some(schema),
                    message: "A mapped schema must declare the complete, non-empty identity \
                              of one logical message."
                        .to_string(),
                }],
            },

            ValidationError::OutboxMessageIdentityArityMismatch {
                outbox,
                schema,
                expected,
                actual,
            } => Diagnostic {
                code: DiagnosticCode::Validation(
                    ValidationCode::OutboxMessageIdentityArityMismatch,
                ),
                severity: Severity::Error,
                subject: Some(outbox.clone()),
                message: format!(
                    "Outbox `{outbox}` maps the message identity of `{schema}` \
                     with {actual} field(s), but other mapped schemas use {expected}."
                ),
                evidence: vec![Evidence {
                    subject: Some(schema),
                    message: "Identity tuple positions correspond across schemas, so every \
                              mapped tuple must have the same arity."
                        .to_string(),
                }],
            },

            ValidationError::OutboxOutsideDataModel {
                transaction,
                effect,
                data_model,
                outbox,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::OutboxOutsideDataModel),
                severity: Severity::Error,
                subject: Some(effect.clone()),
                message: format!(
                    "Outbox write `{effect}` in transaction `{transaction}` targets \
                     outbox `{outbox}`, which does not belong to the transaction's \
                     data model `{data_model}`."
                ),
                evidence: vec![Evidence {
                    subject: Some(outbox),
                    message: "An outbox write is atomic with its containing transaction's \
                              commit, so the destination outbox must belong to the declared \
                              data model; Conseqa never infers a distributed cross-data-model \
                              atomic transaction."
                        .to_string(),
                }],
            },

            ValidationError::OutboxWriteMissingDataModel {
                transaction,
                effect,
                outbox,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::OutboxWriteMissingDataModel),
                severity: Severity::Error,
                subject: Some(effect.clone()),
                message: format!(
                    "Outbox write `{effect}` targets outbox `{outbox}`, but its \
                     transaction `{transaction}` declares no data model."
                ),
                evidence: vec![Evidence {
                    subject: Some(transaction),
                    message: "A transaction admitting an outbox message must declare the \
                              data model that owns the outbox — the atomic boundary the \
                              admission participates in."
                        .to_string(),
                }],
            },

            ValidationError::OutboxWriteOutsideTransaction { operation, effect } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::OutboxWriteOutsideTransaction),
                severity: Severity::Error,
                subject: Some(operation.clone()),
                message: format!(
                    "`{operation}` executes outbox write `{effect}` outside a \
                     transaction."
                ),
                evidence: vec![Evidence {
                    subject: Some(effect),
                    message: "An `OutboxWriteEffect`'s only legal execution site is a \
                              transaction's `write_outbox` step: outside one, no containing \
                              application transaction exists whose commit could make the \
                              admission atomic."
                        .to_string(),
                }],
            },

            ValidationError::OutboxWriteCannotBeIntent {
                transaction,
                effect,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::OutboxWriteCannotBeIntent),
                severity: Severity::Error,
                subject: Some(effect.clone()),
                message: format!(
                    "Transaction `{transaction}` establishes outbox write \
                     `{effect}` as an effect intent."
                ),
                evidence: vec![Evidence {
                    subject: Some(transaction),
                    message: "An outbox message is durable typed application data admitted \
                              atomically with a commit, not a captured effect instance for \
                              later execution; write it with a `write_outbox` step instead."
                        .to_string(),
                }],
            },

            ValidationError::OutboxPartitionSchemaNotAdmitted {
                input,
                outbox,
                schema,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::OutboxPartitionSchemaNotAdmitted),
                severity: Severity::Error,
                subject: Some(input.clone()),
                message: format!(
                    "The outbox runtime of `{input}` maps a partition key for \
                     schema `{schema}`, which outbox `{outbox}` does not admit."
                ),
                evidence: vec![Evidence {
                    subject: Some(outbox),
                    message: format!(
                        "The outbox does not declare schema `{schema}` as a message."
                    ),
                }],
            },

            ValidationError::OutboxPartitionMissingSchema {
                input,
                outbox,
                schema,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::OutboxPartitionMissingSchema),
                severity: Severity::Error,
                subject: Some(input.clone()),
                message: format!(
                    "The outbox runtime of `{input}` declares keyed partitioning \
                     but maps no partition key for `{schema}`, which the input \
                     admits from outbox `{outbox}`."
                ),
                evidence: vec![Evidence {
                    subject: Some(schema),
                    message: "Every message schema admitted through the target input must \
                              map into the common partition-key domain; an unmapped one \
                              would belong to no partition."
                        .to_string(),
                }],
            },

            ValidationError::EmptyOutboxPartitionKey { input, schema } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::EmptyOutboxPartitionKey),
                severity: Severity::Error,
                subject: Some(input.clone()),
                message: format!(
                    "The outbox runtime of `{input}` maps `{schema}` to an empty \
                     partition-key tuple, which names no partition."
                ),
                evidence: Vec::new(),
            },

            ValidationError::OutboxPartitionKeyArityMismatch {
                input,
                schema,
                expected,
                actual,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::OutboxPartitionKeyArityMismatch),
                severity: Severity::Error,
                subject: Some(input.clone()),
                message: format!(
                    "The outbox runtime of `{input}` maps the partition key of \
                     `{schema}` with {actual} field(s), but other mapped schemas \
                     use {expected}."
                ),
                evidence: vec![Evidence {
                    subject: Some(schema),
                    message: "Partition-key tuple positions correspond across schemas, so \
                              every mapped tuple must have the same arity."
                        .to_string(),
                }],
            },

            ValidationError::PartitionOrderingWithoutPartitioning { input } => Diagnostic {
                code: DiagnosticCode::Validation(
                    ValidationCode::PartitionOrderingWithoutPartitioning,
                ),
                severity: Severity::Error,
                subject: Some(input.clone()),
                message: format!(
                    "The outbox runtime of `{input}` declares `ordering: partition` \
                     with `partitioning: none`."
                ),
                evidence: vec![Evidence {
                    subject: Some(input),
                    message: "Partition ordering is an independent order within each keyed \
                              partition; without keyed partitioning no domain exists for \
                              the guarantee to be interpreted over."
                        .to_string(),
                }],
            },

            ValidationError::OutboxRoutingWithoutPartitioning {
                operation,
                input,
                outbox,
            } => Diagnostic {
                code: DiagnosticCode::Validation(
                    ValidationCode::OutboxRoutingWithoutPartitioning,
                ),
                severity: Severity::Error,
                subject: Some(input.clone()),
                message: format!(
                    "`{input}` of `{operation}` dispatches by `partition_key`, but the \
                     outbox runtime declares `partitioning: none` for `{outbox}`."
                ),
                evidence: vec![Evidence {
                    subject: Some(outbox),
                    message: "Routing consumes an already-declared semantic key rather \
                              than inventing one: declare keyed `partitioning`, or omit \
                              the routing block."
                        .to_string(),
                }],
            },
        }
    }
}
