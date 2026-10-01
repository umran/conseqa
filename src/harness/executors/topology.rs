//! The runtime topology builder: conservative defaults, in code (§19 of
//! the System One orchestration revision).
//!
//! L1 proves no transaction property, and only the replay families
//! consume its delivery facts, so its authoring is the safest to
//! mechanize. Derived from L0 alone:
//!
//! - one execution pool per service, of unspecified member concurrency;
//! - one router per request boundary, into its service's pool, with no
//!   routing — no member-affinity fact;
//! - one runtime per subscription: `at_least_once` delivery (a default
//!   never asserts a stronger fact than the prompt supports, and a
//!   replay obligation must not become provable because a default
//!   invented a delivery guarantee), dispatched to its service's pool,
//!   with no transport grouping or ordering of its own;
//! - one runtime per outbox input: unpartitioned, unordered, dispatched
//!   to its service's pool;
//! - one storage layout per data object, partitioned by its identity.
//!
//! Only what is missing is declared: an author's declaration is never
//! replaced. Nothing here asks a model: every default is the weakest
//! fact, and a stated knob — delivery, concurrency, ordering, batching —
//! is the agent's to write when the prompt asks for it. The whole model
//! is assembled, validated and verified with the defaults before they
//! are submitted, and any diagnostic sends the task to a session.

use std::collections::{BTreeMap, BTreeSet};

use uuid::Uuid;

use crate::confluence::{
    CommitRequest, EngineError, Mutation, OperationInterfaceDraft, PatchId, SearchSpec, SpecPatch,
    SymbolKey, SymbolKind,
};
use crate::spec::{
    DataObject, DataObjectRef, DeliverySemantics, ExecutionPool, Id, Input, MemberConcurrency,
    OperationInputRef, OutboxDispatch, OutboxOrdering, OutboxPartitioning, OutboxRuntime, Router,
    StorageLayout, SubscriptionDispatch, SubscriptionRuntime,
};

use super::{Abstention, BuildContext, Built};

pub(super) async fn build(context: &BuildContext<'_>) -> Built {
    match synthesize(context).await {
        Ok(built) => built,

        Err(error) => Built::Abstained(Abstention::because(format!(
            "the engine refused a read: {error}"
        ))),
    }
}

/// Every key of `kind` the task can see, with its rendered content.
fn read_all(
    context: &BuildContext<'_>,
    kind: SymbolKind,
) -> Result<Vec<(SymbolKey, serde_json::Value)>, EngineError> {
    context
        .engine
        .search_symbols(
            context.task,
            &SearchSpec {
                kind: Some(kind),
                ..Default::default()
            },
        )?
        .into_iter()
        .map(|key| {
            let view = context.engine.read_symbol(context.task, &key)?;

            Ok((key, view.content))
        })
        .collect()
}

fn keys(context: &BuildContext<'_>, kind: SymbolKind) -> Result<BTreeSet<SymbolKey>, EngineError> {
    Ok(context
        .engine
        .search_symbols(
            context.task,
            &SearchSpec {
                kind: Some(kind),
                ..Default::default()
            },
        )?
        .into_iter()
        .collect())
}

/// The name a declaration takes from the symbol it realizes.
fn named(prefix: &str, id: &Id, strip: &str) -> Id {
    Id(format!(
        "{prefix}.{}",
        id.0.strip_prefix(strip).unwrap_or(&id.0)
    ))
}

async fn synthesize(context: &BuildContext<'_>) -> Result<Built, EngineError> {
    let task = context.engine.task_context(context.task)?;

    // What is already declared is kept.
    let pools = keys(context, SymbolKind::ExecutionPool)?;
    let routers = read_all(context, SymbolKind::Router)?;
    let subscriptions = keys(context, SymbolKind::SubscriptionRuntime)?;
    let outboxes = keys(context, SymbolKind::OutboxRuntime)?;
    let layouts = read_all(context, SymbolKind::StorageLayout)?;

    let routed: BTreeSet<OperationInputRef> = routers
        .iter()
        .filter_map(|(_, content)| serde_json::from_value::<Router>(content.clone()).ok())
        .map(|router| router.boundary)
        .collect();

    let laid_out: BTreeSet<DataObjectRef> = layouts
        .iter()
        .filter_map(|(_, content)| serde_json::from_value::<StorageLayout>(content.clone()).ok())
        .map(|layout| layout.object)
        .collect();

    let mut mutations: Vec<Mutation> = Vec::new();
    let mut pooled: BTreeMap<Id, Id> = BTreeMap::new();

    let mut pool_of = |service: &Id, mutations: &mut Vec<Mutation>| -> Id {
        pooled
            .entry(service.clone())
            .or_insert_with(|| {
                let pool = named("pool", service, "service.");

                if !pools.contains(&SymbolKey::ExecutionPool(pool.clone())) {
                    mutations.push(Mutation::PutExecutionPool {
                        id: pool.clone(),
                        value: ExecutionPool {
                            member_concurrency: MemberConcurrency::Unspecified,
                        },
                    });
                }

                pool
            })
            .clone()
    };

    for (key, content) in read_all(context, SymbolKind::OperationInterface)? {
        let SymbolKey::OperationInterface(operation) = key else {
            continue;
        };

        let Ok(interface) = serde_json::from_value::<OperationInterfaceDraft>(content) else {
            return Ok(Built::Abstained(Abstention::because(format!(
                "the interface of {operation} could not be read"
            ))));
        };

        for (input_id, input) in &interface.inputs {
            let boundary = OperationInputRef {
                operation: operation.clone(),
                input: input_id.clone(),
            };

            match input {
                Input::Request(_) => {
                    if routed.contains(&boundary) {
                        continue;
                    }

                    let pool = pool_of(&interface.service, &mut mutations);

                    mutations.push(Mutation::PutRouter {
                        id: named("router", &operation, "operation."),
                        value: Router {
                            boundary,
                            pool,
                            routing: None,
                        },
                    });
                }

                Input::Subscription(_) => {
                    if subscriptions.contains(&SymbolKey::SubscriptionRuntime {
                        operation: operation.clone(),
                        input: input_id.clone(),
                    }) {
                        continue;
                    }

                    let pool = pool_of(&interface.service, &mut mutations);

                    mutations.push(Mutation::PutSubscriptionRuntime {
                        operation: operation.clone(),
                        input: input_id.clone(),
                        value: SubscriptionRuntime {
                            delivery: DeliverySemantics::AtLeastOnce,
                            grouping: None,
                            ordering: None,
                            dispatch: SubscriptionDispatch {
                                pool,
                                routing: None,
                            },
                        },
                    });
                }

                Input::Outbox(_) => {
                    if outboxes.contains(&SymbolKey::OutboxRuntime {
                        operation: operation.clone(),
                        input: input_id.clone(),
                    }) {
                        continue;
                    }

                    let pool = pool_of(&interface.service, &mut mutations);

                    mutations.push(Mutation::PutOutboxRuntime {
                        operation: operation.clone(),
                        input: input_id.clone(),
                        value: OutboxRuntime {
                            partitioning: OutboxPartitioning::None,
                            ordering: OutboxOrdering::None,
                            dispatch: OutboxDispatch {
                                pool,
                                routing: None,
                                batching: None,
                            },
                        },
                    });
                }
            }
        }
    }

    for (key, content) in read_all(context, SymbolKind::DataObject)? {
        let SymbolKey::DataObject { data_model, object } = key else {
            continue;
        };

        let reference = DataObjectRef {
            data_model,
            object: object.clone(),
        };

        if laid_out.contains(&reference) {
            continue;
        }

        let Ok(data) = serde_json::from_value::<DataObject>(content) else {
            continue;
        };

        mutations.push(Mutation::PutStorageLayout {
            id: named("layout", &object, "object."),
            value: StorageLayout {
                object: reference,
                partition_key: data.identity,
            },
        });
    }

    if mutations.is_empty() {
        return Ok(Built::NothingToDo {
            summary: "every runtime declaration already exists".to_string(),
        });
    }

    let declared = mutations.len();

    let patch = SpecPatch { mutations };

    let verdict = context
        .engine
        .evaluate_candidate(context.task, &patch, &[])
        .await?;

    if verdict.verified().is_none() {
        return Ok(Built::Abstained(
            Abstention::because("the default runtime topology was refused").with_findings(vec![
                verdict
                    .refusal()
                    .unwrap_or_else(|| "it was not verified".to_string()),
            ]),
        ));
    }

    let outcome = context
        .engine
        .submit(CommitRequest {
            task: context.task,
            patch_id: PatchId::fresh(),
            base_revision: task.snapshot_revision,
            patch,
            client_nonce: Uuid::new_v4(),
        })
        .await?;

    Ok(match outcome {
        Ok(_) => Built::Committed {
            summary: format!("the default runtime topology: {declared} declarations"),
        },

        Err(rejection) if rejection.is_stale_context() => Built::Stale,

        Err(rejection) => Built::Abstained(
            Abstention::because("the gate rejected a topology the analyzer had admitted")
                .with_findings(vec![format!("{rejection:?}")]),
        ),
    })
}
