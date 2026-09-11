//! Analysis integration tests (§102–§103 of the confluence spec):
//! assembly gaps, revision-tagged analysis, coalescing, pinning,
//! validation gating verification, summaries, reports, and tracked
//! context bundles.

// Rejections are rich by design and the commit path is cold.
#![allow(clippy::result_large_err)]

use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use conseqa::confluence::{
    AnalysisHub, AnalysisState, BundleSpec, CommitReceipt, CommitRejection, CommitRequest,
    ConfluenceEngine, CreateTask, EventBus, Mutation, OperationInterfaceDraft, OperationReadMode,
    PatchId, RequirementFamily, RunId, RunMetadata, SpecPatch, SymbolKey, TaskBudget, TaskHandle,
    TaskKind, TaskState, WorkspaceSnapshot, WorkspaceState, WriteGrant, WriteScope,
};
use conseqa::spec::{
    ExecutionPool, Id, Input, MemberConcurrency, MessageSelector, OperationBlock, OperationStep,
    Revision, SubscriptionInput, TransactionStep,
};
use uuid::Uuid;

fn id(text: &str) -> Id {
    Id(text.to_string())
}

fn fixture_workspace() -> WorkspaceState {
    let source =
        std::fs::read_to_string("tests/fixtures/flash_checkout.yaml").expect("fixture exists");

    let model = conseqa::parser::yaml::parse(&source).expect("fixture parses");

    WorkspaceState::from_model(&model, RunMetadata::new(RunId("analysis-test".to_string())))
}

fn task(engine: &ConfluenceEngine, kind: TaskKind, scope: WriteScope) -> TaskHandle {
    engine
        .create_task(CreateTask {
            kind,
            objective: "analysis test task".to_string(),
            write_scope: scope,
            prompt_evidence: Vec::new(),
            budget: TaskBudget::default(),
        })
        .expect("task is created")
}

async fn submit(
    engine: &ConfluenceEngine,
    task: &TaskHandle,
    mutations: Vec<Mutation>,
) -> Result<CommitReceipt, CommitRejection> {
    engine
        .submit(CommitRequest {
            task: task.id,
            patch_id: PatchId::fresh(),
            base_revision: task.snapshot_revision,
            patch: SpecPatch { mutations },
            client_nonce: Uuid::new_v4(),
        })
        .await
        .expect("the sequencer is running")
}

async fn ready(engine: &ConfluenceEngine, revision: Revision) -> AnalysisState {
    tokio::time::timeout(Duration::from_secs(20), engine.analysis_ready(revision))
        .await
        .expect("analysis reaches a terminal state")
}

/// A runtime-topology write: the modern stand-in for a change that
/// belongs to the model but to no operation's summarized contract.
fn put_pool(name: &str, bound: u32) -> Mutation {
    Mutation::PutExecutionPool {
        id: id(name),
        value: ExecutionPool {
            member_concurrency: MemberConcurrency::Bounded(
                NonZeroU32::new(bound).expect("non-zero"),
            ),
            execution_handoff: Some(conseqa::spec::ExecutionHandoff::ExclusiveOwnership),
        },
    }
}

/// Replaces an operation's program with a bare terminal — a real
/// semantic change to the operation, used where a test needs one.
fn truncate_program(operation: &str) -> Mutation {
    Mutation::ReplaceOperationProgram {
        operation: id(operation),
        program: OperationBlock {
            steps: vec![OperationStep::Complete],
        },
    }
}

#[tokio::test]
async fn commits_publish_before_analysis_finishes() {
    let engine = ConfluenceEngine::in_memory(fixture_workspace()).expect("engine starts");

    let a = task(
        &engine,
        TaskKind::TopologySynthesis,
        WriteScope::runtime_topology(),
    );

    let receipt = submit(&engine, &a, vec![put_pool("pool.spare", 2)])
        .await
        .expect("the commit is accepted without waiting for verification");

    // The head is already published...
    assert_eq!(engine.head_revision(), receipt.revision);

    // ...and analysis catches up asynchronously, tagged with exactly
    // this revision.
    let AnalysisState::Ready(analysis) = ready(&engine, receipt.revision).await else {
        panic!("the fixture verifies");
    };

    assert_eq!(analysis.revision, receipt.revision);
    assert_eq!(analysis.obligations.model_revision, Some(receipt.revision.0));
}

#[tokio::test]
async fn analysis_is_tagged_per_revision_and_summaries_abstract_implementations() {
    let engine = ConfluenceEngine::in_memory(fixture_workspace()).expect("engine starts");

    let first = engine.head_revision();

    let AnalysisState::Ready(before) = ready(&engine, first).await else {
        panic!("the fixture verifies");
    };

    let a = task(
        &engine,
        TaskKind::TopologySynthesis,
        WriteScope::runtime_topology(),
    );

    let receipt = submit(&engine, &a, vec![put_pool("pool.spare", 2)])
        .await
        .expect("the commit is accepted");

    let AnalysisState::Ready(after) = ready(&engine, receipt.revision).await else {
        panic!("the new head verifies");
    };

    // Old analysis remains the truth of its own revision.
    assert_eq!(before.revision, first);
    assert_eq!(after.revision, receipt.revision);

    // The runtime-topology change touched no summarized contract or
    // proof of create_order, so its summary hash is unchanged — the
    // module-boundary abstraction of §41. A summary abstracts over the
    // realization as much as over the implementation.
    let summary_before = &before.summaries[&id("operation.create_order")];
    let summary_after = &after.summaries[&id("operation.create_order")];

    assert_eq!(summary_before.summary_hash, summary_after.summary_hash);
    assert_eq!(summary_before.program_hash, summary_after.program_hash);
}

#[tokio::test]
async fn draft_heads_report_precise_assembly_gaps_until_programs_arrive() {
    // A workspace holding one planned operation: interface only.
    let mut workspace = WorkspaceState::empty(RunMetadata::new(RunId("draft".to_string())));

    workspace.services.insert(
        id("service.checkout"),
        conseqa::spec::Service {
            kind: conseqa::spec::ServiceKind::Backend,
        },
    );

    workspace.operations.insert(
        id("operation.noop"),
        conseqa::confluence::DraftOperation::planned(OperationInterfaceDraft {
            service: id("service.checkout"),
            description: None,
            inputs: BTreeMap::new(),
            invocation_lock: None,
        }),
    );

    let engine = ConfluenceEngine::in_memory(workspace).expect("engine starts");

    let initial = engine.head_revision();

    let AnalysisState::NotAssemblable { gaps } = ready(&engine, initial).await else {
        panic!("a program-less draft is not assemblable");
    };

    let gaps_json = serde_json::to_string(&gaps).expect("gaps serialize");

    assert!(gaps_json.contains("missing_program"), "{gaps_json}");
    assert!(gaps_json.contains("operation.noop"), "{gaps_json}");

    // A program is the only thing assembly waits for. The runtime
    // model is never a gap: L1 is optional, so a workspace declaring
    // no topology assembles to a valid L0-only model.
    //
    // Committing the program makes the head assemblable, and only then
    // does the full validator run.
    let a = task(
        &engine,
        TaskKind::OperationSynthesis,
        WriteScope::operation_synthesis(id("operation.noop")),
    );

    let receipt = submit(
        &engine,
        &a,
        vec![truncate_program("operation.noop")],
    )
    .await
    .expect("the synthesis commit is accepted");

    let AnalysisState::Ready(analysis) = ready(&engine, receipt.revision).await else {
        panic!("the assembled model validates and verifies");
    };

    assert_eq!(analysis.revision, receipt.revision);
}

#[tokio::test]
async fn validation_failure_prevents_verification() {
    // Break the fixture structurally in a way draft gates don't judge:
    // remove a schema other declarations reference.
    let mut workspace = fixture_workspace();

    workspace.schemas.remove(&id("schema.OrderCreated"));

    let engine = ConfluenceEngine::in_memory(workspace).expect("engine starts");

    let AnalysisState::ValidationFailed { errors } = ready(&engine, engine.head_revision()).await
    else {
        panic!("a dangling reference fails validation");
    };

    assert!(!errors.is_empty());

    let rendered = serde_json::to_string(&errors).expect("errors serialize");

    assert!(rendered.contains("schema.OrderCreated"), "{rendered}");
}

#[tokio::test]
async fn rapid_revisions_coalesce_but_pinned_ones_run() {
    let workspace = fixture_workspace();
    let events = EventBus::new(64);
    let hub = AnalysisHub::start(events);

    // Flood the hub far faster than full verification can run. The
    // worker analyzes what it dequeues; unpinned queued revisions are
    // replaced by newer arrivals.
    let flood = 40u64;
    let pinned_revision = Revision(5);

    let _pin = hub.pin(pinned_revision);

    // Building graphs dominates enqueueing; do it up front so the
    // enqueue loop outpaces the analysis worker decisively.
    let snapshots: Vec<Arc<WorkspaceSnapshot>> = (2..=flood)
        .map(|offset| {
            let mut workspace = workspace.clone();

            workspace.revision = Revision(offset);

            Arc::new(WorkspaceSnapshot::build(workspace, None))
        })
        .collect();

    for snapshot in snapshots {
        hub.enqueue(snapshot);
    }

    // The newest revision always completes.
    let last = tokio::time::timeout(Duration::from_secs(30), hub.ready(Revision(flood)))
        .await
        .expect("the newest revision is analyzed");

    assert!(matches!(last, AnalysisState::Ready(_)));

    // The pinned one was never coalesced away.
    let pinned = tokio::time::timeout(Duration::from_secs(30), hub.ready(pinned_revision))
        .await
        .expect("the pinned revision is analyzed");

    assert!(matches!(pinned, AnalysisState::Ready(_)));

    // And the flood was actually coalesced: some intermediate,
    // unpinned revisions never ran.
    let skipped = (2..flood)
        .filter(|revision| *revision != pinned_revision.0)
        .filter(|revision| !hub.state(Revision(*revision)).is_terminal())
        .count();

    assert!(
        skipped > 0,
        "at least one queued unpinned revision was coalesced away"
    );
}

#[tokio::test]
async fn requirement_reports_serve_verdicts_and_guard_repairs() {
    let engine = ConfluenceEngine::in_memory(fixture_workspace()).expect("engine starts");

    let head = engine.head_revision();

    ready(&engine, head).await;

    let repair = task(
        &engine,
        TaskKind::RequirementRepair,
        WriteScope::requirement_repair(id("operation.charge_payment")),
    );

    let report = engine
        .requirement_report(repair.id, Some(id("operation.charge_payment")), None)
        .expect("the report is ready");

    assert_eq!(report["analysis"], "ready");

    let obligations = report["obligations"].as_array().expect("obligations listed");

    // charge_payment declares serialization, ordering, and idempotency
    // requirements — and the fixture's card charge is deliberately not
    // deduplicated, so idempotency is unproven.
    assert_eq!(obligations.len(), 3, "{obligations:?}");

    let idempotency = obligations
        .iter()
        .find(|obligation| obligation["property"]["kind"] == "idempotency")
        .expect("the idempotency obligation is present");

    assert_eq!(idempotency["status"], "unknown");

    // Family filtering.
    let filtered = engine
        .requirement_report(
            repair.id,
            Some(id("operation.charge_payment")),
            Some("idempotency"),
        )
        .expect("the filtered report is ready");

    assert_eq!(
        filtered["obligations"].as_array().expect("listed").len(),
        1
    );

    // The scoped report recorded the operation's sub-symbols, so a
    // concurrent change to the operation invalidates the repair task.
    let writer = task(
        &engine,
        TaskKind::OperationSynthesis,
        WriteScope::operation_synthesis(id("operation.charge_payment")),
    );

    submit(&engine, &writer, vec![truncate_program("operation.charge_payment")])
        .await
        .expect("the writer commits");

    assert_eq!(
        engine.task_status(repair.id).unwrap(),
        TaskState::Invalidated
    );
}

#[tokio::test]
async fn proof_summaries_are_read_through_tracked_reads() {
    let engine = ConfluenceEngine::in_memory(fixture_workspace()).expect("engine starts");

    ready(&engine, engine.head_revision()).await;

    let reader = task(
        &engine,
        TaskKind::DependencyReview,
        WriteScope::operation_synthesis(id("operation.transfer_stock")),
    );

    let view = engine
        .read_operation(
            reader.id,
            &id("operation.apply_payment"),
            OperationReadMode::ProofSummary,
        )
        .expect("the summary is served");

    assert!(view.content["summary_hash"].is_string());

    // apply_payment's idempotency is proven in the fixture;
    // create_order's is one of the fixture's deliberate gaps.
    let idempotency = view.content["idempotency"].as_array().expect("entries");

    assert_eq!(idempotency.len(), 1);
    assert_eq!(idempotency[0]["proven"], true);

    // The summary read observes the summary's inputs (§41): changing
    // the summarized operation invalidates the reader.
    let writer = task(
        &engine,
        TaskKind::OperationSynthesis,
        WriteScope::operation_synthesis(id("operation.apply_payment")),
    );

    submit(
        &engine,
        &writer,
        vec![truncate_program("operation.apply_payment")],
    )
    .await
    .expect("the writer commits");

    assert_eq!(
        engine.task_status(reader.id).unwrap(),
        TaskState::Invalidated
    );
}

#[tokio::test]
async fn context_bundles_slice_and_track() {
    let engine = ConfluenceEngine::in_memory(fixture_workspace()).expect("engine starts");

    ready(&engine, engine.head_revision()).await;

    let repair = task(
        &engine,
        TaskKind::RequirementRepair,
        WriteScope::requirement_repair(id("operation.charge_payment")),
    );

    let bundle = engine
        .context_bundle(
            repair.id,
            &BundleSpec {
                operation: Some(id("operation.charge_payment")),
                requirements: vec![(RequirementFamily::Idempotency, 0)],
                include: Vec::new(),
            },
        )
        .expect("the bundle builds");

    assert!(bundle.operation.is_some());

    let shared: Vec<String> = bundle
        .shared_symbols
        .iter()
        .map(|view| view.key.to_string())
        .collect();

    assert!(
        shared.contains(&"topic(topic.order_events)".to_string()),
        "{shared:?}"
    );

    assert!(
        shared.contains(&"schema(schema.ChargeAccepted)".to_string()),
        "{shared:?}"
    );

    assert!(
        shared.contains(&"service(service.payments)".to_string()),
        "{shared:?}"
    );

    // The evidence is the exact obligation under repair.
    let evidence = bundle
        .analyzer_evidence
        .first()
        .expect("evidence is attached");

    assert_eq!(
        evidence["id"], "oblig.operation.charge_payment.idempotency.0",
        "{evidence}"
    );

    assert_eq!(evidence["status"], "unknown");

    // Everything the bundle included was recorded: a change to a
    // sliced shared symbol invalidates the task.
    let coordinator = task(
        &engine,
        TaskKind::SharedDependencyRepair,
        WriteScope::of([WriteGrant::TopLevelSymbol(SymbolKey::Schema(id(
            "schema.ChargeDeclined",
        )))]),
    );

    let mut declined = fixture_workspace().schemas[&id("schema.ChargeDeclined")].clone();

    if let conseqa::spec::Schema::Canonical(canonical) = &mut declined {
        canonical.description = Some("Amended.".to_string());
    }

    submit(
        &engine,
        &coordinator,
        vec![Mutation::PutSchema {
            id: id("schema.ChargeDeclined"),
            value: declined,
        }],
    )
    .await
    .expect("the coordinator amends the schema");

    assert_eq!(
        engine.task_status(repair.id).unwrap(),
        TaskState::Invalidated
    );
}

#[tokio::test]
async fn bundles_fall_back_to_interfaces_before_analysis_and_use_summaries_after() {
    // A workspace where charge_payment calls a request target, so the
    // bundle has a callee: reuse the fixture and add a gateway that
    // calls create_order.
    let mut workspace = fixture_workspace();

    workspace.operations.insert(
        id("operation.gateway"),
        conseqa::confluence::DraftOperation::planned(OperationInterfaceDraft {
            service: id("service.checkout"),
            description: None,
            inputs: BTreeMap::from([(
                id("input.gateway.paid"),
                Input::Subscription(SubscriptionInput {
                    topic: id("topic.order_events"),
                    messages: MessageSelector::Only(
                        [id("schema.OrderPaid")].into_iter().collect(),
                    ),
                    acknowledge_on_success: None,
                }),
            )]),
            invocation_lock: None,
        }),
    );

    {
        let draft = workspace.operations.get_mut(&id("operation.gateway")).unwrap();

        draft.program = Some(OperationBlock {
            steps: vec![
                OperationStep::ExecuteEffect(conseqa::spec::ExecuteEffect {
                    effect_id: id("effect.gateway.create_order"),
                    effect: conseqa::spec::Effect::Request(conseqa::spec::RequestEffect {
                        target: conseqa::spec::RequestTarget {
                            operation: id("operation.create_order"),
                            input: id("input.create_order.request"),
                        },
                        schema: id("schema.CreateOrderRequest"),
                        retry: conseqa::spec::RetrySemantics::Unspecified,
                        idempotency_key_propagation: Vec::new(),
                    }),
                    values: conseqa::spec::Derivation::Unspecified,
                    bind: None,
                }),
                OperationStep::Complete,
            ],
        });

        draft.recompute_stage();
    }

    let engine = ConfluenceEngine::in_memory(workspace).expect("engine starts");

    // Before analysis is ready, the bundle carries the callee's
    // interface among the shared symbols.
    let early = task(
        &engine,
        TaskKind::OperationSynthesis,
        WriteScope::operation_synthesis(id("operation.gateway")),
    );

    let bundle = engine
        .context_bundle(
            early.id,
            &BundleSpec {
                operation: Some(id("operation.gateway")),
                requirements: Vec::new(),
                include: Vec::new(),
            },
        )
        .expect("the bundle builds");

    if bundle.dependency_summaries.is_empty() {
        let shared: Vec<String> = bundle
            .shared_symbols
            .iter()
            .map(|view| view.key.to_string())
            .collect();

        assert!(
            shared.contains(&"operation_interface(operation.create_order)".to_string()),
            "{shared:?}"
        );
    }

    // Once analysis is ready, the bundle carries the callee's proof
    // summary.
    ready(&engine, engine.head_revision()).await;

    let late = task(
        &engine,
        TaskKind::OperationSynthesis,
        WriteScope::operation_synthesis(id("operation.gateway")),
    );

    let bundle = engine
        .context_bundle(
            late.id,
            &BundleSpec {
                operation: Some(id("operation.gateway")),
                requirements: Vec::new(),
                include: Vec::new(),
            },
        )
        .expect("the bundle builds");

    assert_eq!(bundle.dependency_summaries.len(), 1);
    assert_eq!(
        bundle.dependency_summaries[0].operation,
        id("operation.create_order")
    );
}

fn create_order_program(engine: &ConfluenceEngine) -> OperationBlock {
    engine
        .head_snapshot()
        .workspace
        .operations
        .get(&id("operation.create_order"))
        .expect("create_order exists")
        .program
        .clone()
        .expect("create_order has a program")
}

/// The draft gate rejects a structurally broken program at submit time —
/// a fixable, in-session rejection, never a stale-context restart — and
/// the very same task then commits the corrected program. This is the
/// convergence fix: a dangling effect intent is caught before it commits,
/// where the agent can still repair it, rather than committing and only
/// failing whole-model validation asynchronously.
#[tokio::test]
async fn a_broken_program_is_rejected_in_session_then_repaired() {
    let engine = ConfluenceEngine::in_memory(fixture_workspace()).expect("engine starts");

    let valid = create_order_program(&engine);

    // Drop the establishment, leaving the execution referring to an
    // effect intent nothing produces.
    let mut broken = valid.clone();

    for step in &mut broken.steps {
        if let OperationStep::Transaction(transaction) = step
            && transaction.id == id("tx.create_order.new")
        {
            transaction
                .steps
                .retain(|inner| !matches!(inner, TransactionStep::EstablishEffectIntent(_)));
        }
    }

    let synth = task(
        &engine,
        TaskKind::OperationSynthesis,
        WriteScope::operation_synthesis(id("operation.create_order")),
    );

    // Observe the operation's shared symbols so read-before-reference is
    // satisfied and the commit reaches draft validation.
    engine
        .context_bundle(
            synth.id,
            &BundleSpec {
                operation: Some(id("operation.create_order")),
                requirements: Vec::new(),
                include: Vec::new(),
            },
        )
        .expect("the bundle builds");

    let rejection = submit(
        &engine,
        &synth,
        vec![Mutation::ReplaceOperationProgram {
            operation: id("operation.create_order"),
            program: broken,
        }],
    )
    .await
    .expect_err("a dangling effect intent is rejected");

    match &rejection {
        CommitRejection::DraftValidationFailed { diagnostics } => assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("effect intent")),
            "expected a dangling-effect-intent diagnostic, got:\n{diagnostics:#?}"
        ),

        other => panic!("expected DraftValidationFailed, got {other:?}"),
    }

    // Not stale context: the task stays runnable, so the agent fixes the
    // program and commits in the same session.
    assert!(!rejection.is_stale_context());

    submit(
        &engine,
        &synth,
        vec![Mutation::ReplaceOperationProgram {
            operation: id("operation.create_order"),
            program: valid,
        }],
    )
    .await
    .expect("the corrected program commits in the same session");
}
