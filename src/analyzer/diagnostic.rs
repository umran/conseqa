use crate::spec::Id;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub severity: Severity,

    /// Primary model entity to which this diagnostic applies.
    pub subject: Option<Id>,

    pub message: String,

    /// Additional model facts that explain the diagnostic.
    pub evidence: Vec<Evidence>,
}

impl Diagnostic {
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    pub subject: Option<Id>,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticCode {
    Validation(ValidationCode),
    Verification(VerificationCode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationCode {
    /// A declared serialization requirement is not established by the
    /// declared facts. Epistemic, not a violation (§1.2).
    SerializationUnproven,

    /// A declared result-replay obligation is not established by the
    /// declared facts. Epistemic, not a violation (§1.2).
    ResultReplayUnproven,

    /// A declared recoverability requirement is not established by the
    /// declared facts. Epistemic, not a violation (§1.2).
    RecoverabilityUnproven,

    /// A declared idempotency requirement is not established by the
    /// declared facts. Epistemic, not a violation (§1.2).
    IdempotencyUnproven,

    /// A recoverability requirement is proven with completion
    /// guaranteed by retries, but no idempotency requirement keyed
    /// from the triggering input declares those retries safe. A
    /// consistency warning, not a verdict.
    RecoverabilityRetrySafetyUndeclared,

    /// A declared ordering requirement is not established by the
    /// declared facts. Epistemic, not a violation (§1.2).
    OrderingUnproven,

    /// A subscription admits duplicate deliveries and its operation
    /// declares no idempotency requirement keyed from it, so the work
    /// a duplicate repeats is checked by nothing. A warning, not a
    /// verdict.
    DuplicateDeliveryUnchecked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationCode {
    DuplicateId,
    UnknownReference,
    InvalidReferenceKind,
    InvalidReferenceOwner,

    InvalidFieldPath,
    ValueSourceHasNoSchema,
    ValueSourceOutOfScope,

    FragmentCycle,

    DataObjectSchemaNotCanonical,

    SubscriptionMessageNotOnTopic,
    PublicationEffectMessageNotOnTopic,

    MessageIdentitySchemaNotOnTopic,
    EmptyMessageIdentity,
    MessageIdentityArityMismatch,
    EmptyRequestIdentity,

    TransactionObjectOutsideDataModel,
    TransactionMissingDataModel,

    TransactionReadOutsideTransaction,
    TransactionReadOutOfOrder,
    TransactionReadFieldNotSelected,

    StateTransitionSubjectMismatch,
    TransitionEffectIntentsMismatch,

    EmptyObjectIdentity,

    InvalidInputKind,

    ProgramNotTerminated,
    UnreachableProgramStep,
    TransactionArtifactNotAvailable,
    EffectResultNotBound,
    EffectResultVariantOutOfScope,
    EffectHasNoResult,

    // L1 — runtime topology.
    EmptyRoutingKey,
    RoutingWithoutGrouping,
    DuplicateRouterForBoundary,
    EmptyPartitionKey,
    DuplicateStorageLayoutForObject,

    // L1 — transport grouping and ordering.
    GroupingKeySchemaNotOnTopic,
    GroupingKeyMissingSchema,
    EmptyGroupingKey,
    GroupingKeyArityMismatch,
    WithinGroupWithoutGrouping,
    TransportSemanticsAtBothScopes,
}
