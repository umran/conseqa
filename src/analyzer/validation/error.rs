use crate::analyzer::{
    Diagnostic, DiagnosticCode, Evidence, IdDeclaration, Severity, ValidationCode,
};
use crate::spec::{FieldPath, Id, ResultVariant, StepLocation};

use super::{InputKind, ReferenceKind};

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
    ExternalIdempotencyRequiresIdentity { effect: Id },

    /// An external effect declares `result_replay: replay_stable`
    /// without a keyed interaction identity: the fixed terminal result
    /// is a fact about one interaction, and no identity defines it.
    ExternalReplayStabilityRequiresIdentity { effect: Id },

    /// An external effect declares a `result_replay` behaviour —
    /// `unstable` or `replay_stable` — while declaring no result
    /// contract: there is no modeled synchronous result whose replay
    /// behaviour could be described.
    ExternalResultReplayWithoutResult { effect: Id },

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

    /// An outbox input selects a schema the outbox does not admit.
    OutboxInputMessageNotAdmitted {
        input: Id,
        outbox: Id,
        schema: Id,
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

            ValidationError::OutboxInputMessageNotAdmitted {
                input,
                outbox,
                schema,
            } => Diagnostic {
                code: DiagnosticCode::Validation(ValidationCode::OutboxInputMessageNotAdmitted),
                severity: Severity::Error,
                subject: Some(input.clone()),
                message: format!(
                    "Outbox input `{input}` selects schema `{schema}`, which \
                     outbox `{outbox}` does not admit."
                ),
                evidence: vec![Evidence {
                    subject: Some(outbox),
                    message: format!(
                        "The outbox does not declare schema `{schema}` as a message."
                    ),
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
        }
    }
}
