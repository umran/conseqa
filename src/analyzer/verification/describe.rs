//! Shared rendering helpers for verification diagnostics.

use crate::analyzer::Evidence;
use crate::spec::{Id, ValueRef, ValueSource};

use super::paths::{DecisionTaken, PathRef};
use super::replay::{
    DecisionGap, GoverningKeyDefect, InstanceGap, PayloadIdentityGap, ReplayGap, ResultGap,
    StabilityGap, UnstableRoot,
};

pub(crate) fn governing_key_evidence(defect: &GoverningKeyDefect) -> Evidence {
    match defect {
        GoverningKeyDefect::Empty => Evidence {
            subject: None,
            message: "The governing key is empty: it places every attempt in one \
                      class, and essentially nothing is replay-stable relative to \
                      it."
            .to_string(),
        },

        GoverningKeyDefect::ComponentNotFromInput { source } => Evidence {
            subject: None,
            message: format!(
                "A governing-key component is sourced from {}, which cannot define \
                 a pre-execution equivalence class.",
                describe_value_source(source)
            ),
        },

        GoverningKeyDefect::ComponentsFromMultipleInputs { first, second } => Evidence {
            subject: None,
            message: format!(
                "The governing key mixes components of inputs `{first}` and \
                 `{second}`; no single triggering input defines the population."
            ),
        },

        GoverningKeyDefect::InputNotDeclared { input } => Evidence {
            subject: Some(input.clone()),
            message: format!(
                "The governing key names input `{input}`, which the operation \
                 does not declare."
            ),
        },
    }
}

pub(crate) fn gap_sentences(gaps: &[ReplayGap]) -> String {
    if gaps.is_empty() {
        return "no gaps recorded".to_string();
    }

    gaps.iter().map(gap_sentence).collect::<Vec<_>>().join("; ")
}

fn gap_sentence(gap: &ReplayGap) -> String {
    match gap {
        ReplayGap::NoKeyedCommit => {
            "the transaction declares no keyed commit deduplication".to_string()
        }

        ReplayGap::CommitKeyRootUnstable { root, gap } => format!(
            "commit-key component `{}` is not replay-stable ({})",
            root.path,
            stability_sentence(gap)
        ),

        ReplayGap::ContainsTransition => {
            "the transaction applies a state transition, which V1 never replays naturally"
                .to_string()
        }

        ReplayGap::ContainsInsert => "the transaction inserts an object, and \
                                      duplicate-identity insert outcomes are not yet \
                                      defined"
            .to_string(),

        ReplayGap::ContainsDelete => {
            "the transaction deletes objects, and deletion replay outcomes are not defined"
                .to_string()
        }

        ReplayGap::MutationTargetRootUnstable { root, gap } => format!(
            "a mutation target depends on `{}`, which is not replay-stable ({})",
            root.path,
            stability_sentence(gap)
        ),

        ReplayGap::MutationDerivationUnspecified => {
            "a mutation declares no value provenance".to_string()
        }

        ReplayGap::MutationDerivationRootUnstable { root, gap } => format!(
            "a mutation value depends on `{}`, which is not replay-stable ({})",
            root.path,
            stability_sentence(gap)
        ),

        ReplayGap::ArtifactDerivationUnspecified => {
            "the artifact's establishment declares no value provenance".to_string()
        }

        ReplayGap::ArtifactDerivationRootUnstable { root, gap } => format!(
            "the artifact's values depend on `{}`, which is not replay-stable ({})",
            root.path,
            stability_sentence(gap)
        ),
    }
}

pub(crate) fn stability_sentence(gap: &StabilityGap) -> String {
    match gap {
        StabilityGap::UnidentifiedPayloadField { input, identity } => match identity {
            PayloadIdentityGap::NotDeclared => {
                format!("no declared identity makes non-key fields of `{input}` replay-stable")
            }

            PayloadIdentityGap::SchemaNotMapped { schema } => {
                format!("no message identity is declared for admitted schema `{schema}`")
            }

            PayloadIdentityGap::NotPinnedByKey { schema, field } => match schema {
                Some(schema) => format!(
                    "identity field `{field}` of `{schema}` is not pinned by the \
                     governing key"
                ),

                None => format!(
                    "the declared identity field `{field}` is not pinned by the \
                     governing key"
                ),
            },
        },

        StabilityGap::NotTriggeringInput { input } => format!(
            "it belongs to input `{input}`, which does not trigger the key-bearing \
             invocations"
        ),

        StabilityGap::MutableSubjectState { machine } => {
            format!("it reads mutable state governed by `{machine}`")
        }

        StabilityGap::EffectPayloadRoot { effect } => {
            format!("effect payloads such as `{effect}` are not stable roots in V1")
        }

        StabilityGap::TransactionReadRoot { read } => {
            format!("it reaches transaction read `{read}`, which is never replay-stable")
        }

        StabilityGap::ArtifactUnavailable {
            artifact,
            transaction,
            recovery,
            reconstruction,
        } => format!(
            "artifact `{artifact}`, established by `{transaction}`, is replay-available \
             through neither route; recovery: {}; reconstruction: {}",
            gap_sentences(recovery),
            gap_sentences(reconstruction)
        ),

        StabilityGap::ArtifactNotInContext { artifact } => {
            format!("artifact `{artifact}` is not established before this point of the path")
        }

        StabilityGap::ResultNotInContext { result } => {
            format!("result `{result}` is not bound before this point of the path")
        }

        StabilityGap::ResultUnstable {
            result,
            effect,
            gap,
        } => format!(
            "effect result `{result}` of `{effect}` is not replay-stable: {}",
            result_gap_sentence(gap)
        ),
    }
}

pub(crate) fn result_gap_sentence(gap: &ResultGap) -> String {
    match gap {
        ResultGap::InstanceNotClassFixed { gap } => {
            format!(
                "the instance is not class-fixed ({})",
                instance_gap_sentence(gap)
            )
        }

        ResultGap::RequestSchemaMismatch { expected, actual } => format!(
            "the request declares schema `{actual}`, but the targeted input declares \
             `{expected}`, so payload equality does not transfer"
        ),

        ResultGap::TargetResultNotDeclared { operation, input } => format!(
            "`{operation}` declares no replay-consistent result requirement keyed from \
             `{input}`"
        ),

        ResultGap::TargetResultUnproven { operation, input } => format!(
            "the replay-consistent result requirement of `{operation}` keyed from \
             `{input}` is not proven in this analysis"
        ),

        ResultGap::ExternalNotDeduplicated => {
            "the external boundary is explicitly `not_deduplicated`, so no \
             same-key terminal result is fixed"
                .to_string()
        }

        ResultGap::ExternalDeduplicationUnknown => {
            "no deduplication fact is declared for the external boundary, so \
             nothing identifies same-key executions as one logical interaction \
             with one terminal result"
                .to_string()
        }

        ResultGap::ExternalDeduplicationKeyUnstable { roots } => format!(
            "the external deduplication key is not replay-stable, so attempts \
             may address different logical external interactions: {}",
            unstable_roots(roots)
        ),

        ResultGap::ExternalErrorRetryable => {
            "the observed error is declared retryable — an attempt-level, \
             nonterminal outcome — so it does not establish the logical \
             interaction's terminal result, and a later attempt may observe a \
             different one"
                .to_string()
        }

        ResultGap::ExternalErrorDispositionUnspecified => {
            "the error's disposition is unspecified: no declared fact says \
             whether observing it terminally resolves the logical external \
             interaction"
                .to_string()
        }

        ResultGap::NoResultContract => {
            "the effect contract yields no synchronous result".to_string()
        }

        ResultGap::RaceWinnerNondeterministic { candidates } => {
            let candidates = candidates
                .iter()
                .map(|candidate| format!("`{candidate}`"))
                .collect::<Vec<_>>()
                .join(", ");

            format!(
                "the result is bound by a `race` over [{candidates}]: which candidate \
                 completes first is scheduling nondeterminism, so retries are not \
                 established to observe the same winner"
            )
        }
    }
}

pub(crate) fn instance_gap_sentence(gap: &InstanceGap) -> String {
    match gap {
        InstanceGap::DerivationUnspecified => "its instance provenance is unspecified".to_string(),

        InstanceGap::RootsUnstable { roots } => format!(
            "its instance depends on roots that are not replay-stable: {}",
            unstable_roots(roots)
        ),

        InstanceGap::IntentNotEstablished { intent } => {
            format!("intent `{intent}` is established by no earlier step of the path")
        }

        InstanceGap::IntentNotReplayAvailable {
            intent,
            transaction,
            recovery,
            reconstruction,
        } => format!(
            "intent `{intent}` is established by `{transaction}` but replay-available \
             through neither route; recovery: {}; reconstruction: {}",
            gap_sentences(recovery),
            gap_sentences(reconstruction)
        ),
    }
}

pub(crate) fn decision_gap_sentence(gap: &DecisionGap) -> String {
    match gap {
        DecisionGap::ConditionUnspecified => {
            "the condition declares no fact about how the decision is made".to_string()
        }

        DecisionGap::ConditionRootsUnstable { roots } => format!(
            "the condition depends on roots that are not replay-stable: {}",
            unstable_roots(roots)
        ),

        DecisionGap::ResultNotInContext { result } => {
            format!("result `{result}` is bound by no earlier step of the path")
        }

        DecisionGap::ResultUnstable {
            result,
            effect,
            gap,
        } => format!(
            "result `{result}` of `{effect}` is not replay-stable: {}",
            result_gap_sentence(gap)
        ),
    }
}

pub(crate) fn unstable_roots(roots: &[UnstableRoot]) -> String {
    roots
        .iter()
        .map(|entry| format!("`{}` ({})", entry.root.path, stability_sentence(&entry.gap)))
        .collect::<Vec<_>>()
        .join("; ")
}

/// A decision as a sentence names it.
pub(crate) fn describe_decision(taken: &DecisionTaken) -> String {
    match taken {
        DecisionTaken::Match {
            location,
            result,
            arm,
        } => format!("the match on `{result}` at step `{location}`, taking its `{arm}` arm"),

        DecisionTaken::Branch { location, arm } => {
            format!("the branch at step `{location}`, taking its `{arm}` arm")
        }
    }
}

/// A path as a sentence names it: the program itself when it has no
/// decisions, else the arms taken.
pub(crate) fn describe_path(path: &PathRef) -> String {
    if path.decisions.is_empty() {
        return "the program".to_string();
    }

    format!("the path `{}`", path_label(path))
}

/// The compact label of a path: `ok(result.payment) › then(step 3)`.
pub fn path_label(path: &PathRef) -> String {
    path.decisions
        .iter()
        .map(|decision| match decision {
            DecisionTaken::Match { result, arm, .. } => format!("{arm}({result})"),
            DecisionTaken::Branch { location, arm } => format!("{arm}(step {location})"),
        })
        .collect::<Vec<_>>()
        .join(" › ")
}

pub(crate) fn describe_value_ref(value: &ValueRef) -> String {
    format!(
        "the key (`{}` of {})",
        value.path,
        describe_value_source(&value.source)
    )
}

pub(crate) fn describe_value_source(source: &ValueSource) -> String {
    match source {
        ValueSource::Input(id) => format!("input `{id}`"),
        ValueSource::Effect(id) => format!("effect `{id}`"),
        ValueSource::TransactionOutput(id) => format!("transaction output `{id}`"),
        ValueSource::StateMachineSubject(id) => format!("state-machine subject `{id}`"),
        ValueSource::TransactionRead(id) => format!("transaction read `{id}`"),
        ValueSource::EffectResultOk(id) => format!("the ok payload of effect result `{id}`"),
        ValueSource::EffectResultErr(id) => format!("the err payload of effect result `{id}`"),
    }
}

pub(crate) fn value_source_id(source: &ValueSource) -> Option<&Id> {
    Some(source.id())
}
