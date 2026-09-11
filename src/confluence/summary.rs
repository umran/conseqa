//! Operation proof summaries: the deterministic derived abstraction
//! downstream agents read instead of callee internals (§40–§41 of the
//! confluence spec).
//!
//! A summary carries the operation's caller-facing contracts and one
//! line per declared requirement with its verdict. `summary_hash` is
//! the module boundary: when a callee's program changes but its
//! summary hash does not, a dependent conceptually need not
//! reconsider the callee. V1 still invalidates conservatively on
//! summary *inputs* (§41); the hash is the hook for the later
//! optimization.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::analyzer::verification::{
    IdempotencyVerdict, OrderingVerdict, RecoverabilityVerdict, ResultReplayVerdict,
    SerializationVerdict, VerificationReport,
};
use crate::spec::{
    DeliverySemantics, ErrorDisposition, ExternalIdempotency, ExternalResultReplay, Id, Input,
    Model, Operation,
    RequestIdentity, ResultReplayRequirement, RetrySemantics, ValueRef,
};

use super::fingerprint::SemanticHash;
use super::workspace::OperationInterfaceDraft;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationSummary {
    pub operation: Id,

    /// Hash of the caller-facing interface — identical to the
    /// `OperationInterface` symbol fingerprint.
    pub interface_hash: SemanticHash,

    /// Hash of the program — identical to the `OperationProgram`
    /// symbol fingerprint.
    pub program_hash: SemanticHash,

    pub input_contracts: BTreeMap<Id, InputContract>,
    pub outward_effects: Vec<OutwardEffectContract>,

    pub serialization: Vec<SummaryRequirement>,
    pub ordering: Vec<SummaryRequirement>,
    pub idempotency: Vec<SummaryRequirement>,
    pub result_replay: Vec<SummaryRequirement>,
    pub recoverability: Vec<SummaryRequirement>,

    /// Hash over everything above: the module-boundary identity.
    pub summary_hash: SemanticHash,
}

/// The caller-facing contract of one input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputContract {
    Request {
        schema: Id,
        identity: RequestIdentity,
        result_ok: Id,
        result_err: Id,
        err_disposition: ErrorDisposition,
    },

    Subscription {
        topic: Id,
        delivery: DeliverySemantics,
    },

    Outbox {
        outbox: Id,
        delivery: DeliverySemantics,
        acknowledge_on_success: bool,
    },
}

/// One outward effect contract the operation's program declares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutwardEffectContract {
    Publishes {
        effect: Id,
        topic: Id,
        schema: Id,
    },

    Requests {
        effect: Id,
        operation: Id,
        input: Id,
        schema: Id,
        retry: RetrySemantics,
    },

    External {
        effect: Id,
        name: String,
        identity: ExternalIdentitySummary,
        idempotency: ExternalIdempotency,
        result_replay: ExternalResultReplay,
    },

    /// A transactional outbox write: admitted atomically with the
    /// named transaction's commit.
    WritesOutbox {
        effect: Id,
        transaction: Id,
        outbox: Id,
        schema: Id,
    },
}

/// Whether an external boundary declares an interaction identity —
/// the summary carries the fact, not the key's value references.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalIdentitySummary {
    Unspecified,
    Keyed,
}

/// One declared requirement and its analyzer verdict, as a summary
/// line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SummaryRequirement {
    /// Human-readable key description.
    pub key: String,

    pub proven: bool,

    /// The obstacle's one-line message when unproven.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obstacle: Option<String>,
}

/// Derives the summary for every operation of a verified model.
pub fn derive_summaries(
    model: &Model,
    verification: &VerificationReport,
) -> BTreeMap<Id, OperationSummary> {
    model
        .operations
        .iter()
        .map(|(id, operation)| (id.clone(), derive_one(model, id, operation, verification)))
        .collect()
}

fn derive_one(
    model: &Model,
    id: &Id,
    operation: &Operation,
    verification: &VerificationReport,
) -> OperationSummary {
    let interface_hash = SemanticHash::of(&OperationInterfaceDraft {
        service: operation.service.clone(),
        description: operation.description.clone(),
        inputs: operation.inputs.clone(),
    });

    // JSON serializes `Some(program)` exactly as `program`, so this
    // matches the `OperationProgram` symbol fingerprint, which hashes
    // the draft's `Option`.
    let program_hash = SemanticHash::of(&operation.program);

    let input_contracts = operation
        .inputs
        .iter()
        .map(|(input_id, input)| {
            let contract = match input {
                Input::Request(request) => InputContract::Request {
                    schema: request.schema.clone(),
                    identity: request.identity.clone(),
                    result_ok: request.result.ok.clone(),
                    result_err: request.result.err.schema.clone(),
                    err_disposition: request.result.err.disposition,
                },

                Input::Subscription(subscription) => InputContract::Subscription {
                    topic: subscription.topic.clone(),
                    delivery: model.delivery(id, input_id),
                },

                Input::Outbox(input) => InputContract::Outbox {
                    outbox: input.outbox.clone(),
                    delivery: model.outbox_delivery(id, input_id),
                    acknowledge_on_success: input.acknowledge_on_success,
                },
            };

            (input_id.clone(), contract)
        })
        .collect();

    let outward_effects = operation
        .program
        .effect_declarations()
        .into_iter()
        .map(|(effect_id, effect)| match effect {
            crate::spec::Effect::Publication(publication) => OutwardEffectContract::Publishes {
                effect: effect_id.clone(),
                topic: publication.topic.clone(),
                schema: publication.schema.clone(),
            },

            crate::spec::Effect::Request(request) => OutwardEffectContract::Requests {
                effect: effect_id.clone(),
                operation: request.target.operation.clone(),
                input: request.target.input.clone(),
                schema: request.schema.clone(),
                retry: request.retry,
            },

            crate::spec::Effect::External(external) => OutwardEffectContract::External {
                effect: effect_id.clone(),
                name: external.name.clone(),
                identity: match &external.identity {
                    crate::spec::ExternalIdentity::Unspecified => {
                        ExternalIdentitySummary::Unspecified
                    }
                    crate::spec::ExternalIdentity::Keyed { .. } => ExternalIdentitySummary::Keyed,
                },
                idempotency: external.idempotency,
                result_replay: external.result_replay,
            },

            // Structurally invalid at a direct site — validation
            // rejects the model — but the summary stays total.
            crate::spec::Effect::OutboxWrite(write) => OutwardEffectContract::WritesOutbox {
                effect: effect_id.clone(),
                transaction: Id(String::new()),
                outbox: write.outbox.clone(),
                schema: write.schema.clone(),
            },
        })
        .chain(operation.program.outbox_write_declarations().into_iter().map(
            |(transaction_id, write)| OutwardEffectContract::WritesOutbox {
                effect: write.effect_id.clone(),
                transaction: transaction_id.clone(),
                outbox: write.effect.outbox.clone(),
                schema: write.effect.schema.clone(),
            },
        ))
        .collect();

    let serialization = operation
        .requirements
        .serialization
        .iter()
        .enumerate()
        .map(|(index, requirement)| {
            let check = verification
                .serialization
                .iter()
                .find(|check| &check.operation == id && check.requirement == index);

            SummaryRequirement {
                key: value_ref_label(&requirement.key),
                proven: matches!(
                    check.map(|check| &check.verdict),
                    Some(SerializationVerdict::Proven { .. })
                ),
                obstacle: obstacle(check.and_then(|check| check.diagnostic())),
            }
        })
        .collect();

    let ordering = operation
        .requirements
        .ordering
        .iter()
        .enumerate()
        .map(|(index, requirement)| {
            let check = verification
                .ordering
                .iter()
                .find(|check| &check.operation == id && check.requirement == index);

            SummaryRequirement {
                key: value_ref_label(&requirement.key),
                proven: matches!(
                    check.map(|check| &check.verdict),
                    Some(OrderingVerdict::Proven { .. })
                ),
                obstacle: obstacle(check.and_then(|check| check.diagnostic())),
            }
        })
        .collect();

    let idempotency = operation
        .requirements
        .idempotency
        .iter()
        .enumerate()
        .map(|(index, requirement)| {
            let check = verification
                .idempotency
                .iter()
                .find(|check| &check.operation == id && check.requirement == index);

            SummaryRequirement {
                key: key_label(&requirement.key.components),
                proven: matches!(
                    check.map(|check| &check.verdict),
                    Some(IdempotencyVerdict::Proven { .. })
                ),
                obstacle: obstacle(check.and_then(|check| check.diagnostic())),
            }
        })
        .collect();

    let result_replay = operation
        .requirements
        .idempotency
        .iter()
        .enumerate()
        .filter(|(_, requirement)| requirement.result == ResultReplayRequirement::ReplayConsistent)
        .map(|(index, requirement)| {
            let check = verification
                .result_replay
                .iter()
                .find(|check| &check.operation == id && check.requirement == index);

            SummaryRequirement {
                key: key_label(&requirement.key.components),
                proven: matches!(
                    check.map(|check| &check.verdict),
                    Some(ResultReplayVerdict::Proven { .. })
                ),
                obstacle: obstacle(check.and_then(|check| check.diagnostic())),
            }
        })
        .collect();

    let recoverability = operation
        .requirements
        .recoverability
        .iter()
        .enumerate()
        .map(|(index, requirement)| {
            let check = verification
                .recoverability
                .iter()
                .find(|check| &check.operation == id && check.requirement == index);

            SummaryRequirement {
                key: key_label(&requirement.key.components),
                proven: matches!(
                    check.map(|check| &check.verdict),
                    Some(RecoverabilityVerdict::Proven { .. })
                ),
                obstacle: obstacle(check.and_then(|check| check.diagnostic())),
            }
        })
        .collect();

    let summary_hash = SemanticHash::of(&(
        &interface_hash,
        &program_hash,
        &input_contracts,
        &outward_effects,
        &serialization,
        &ordering,
        &idempotency,
        &result_replay,
        &recoverability,
    ));

    OperationSummary {
        operation: id.clone(),
        interface_hash,
        program_hash,
        input_contracts,
        outward_effects,
        serialization,
        ordering,
        idempotency,
        result_replay,
        recoverability,
        summary_hash,
    }
}

fn obstacle(diagnostic: Option<crate::analyzer::Diagnostic>) -> Option<String> {
    diagnostic.map(|diagnostic| diagnostic.message)
}

fn value_ref_label(value: &ValueRef) -> String {
    format!("{}.{}", value.source.id(), value.path)
}

fn key_label(components: &[ValueRef]) -> String {
    if components.is_empty() {
        return "(empty key)".to_string();
    }

    components
        .iter()
        .map(value_ref_label)
        .collect::<Vec<_>>()
        .join(" + ")
}
