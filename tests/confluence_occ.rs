//! OCC commit-engine tests (§100–§102 of the confluence spec):
//! non-conflicting commits, read-write / write-write conflicts,
//! phantoms, removed dependencies, unobserved references, duplicate
//! submission, task invalidation, and draft workspace behavior.

// Rejections are rich by design and the commit path is cold.
#![allow(clippy::result_large_err)]

use std::collections::{BTreeMap, BTreeSet};

use conseqa::confluence::{
    CommitReceipt, CommitRejection, CommitRequest, ConfluenceEngine, CreateTask, EngineError,
    EngineEvent, GraphQuery, InvalidationCause, Mutation, OperationDraftStage,
    OperationInterfaceDraft, OperationReadMode, PatchId, PromptObligation, PromptObligationId,
    PromptObligationStatus, ProposalStatus, ProposedRequirement, RequirementOrigin,
    RequirementSubmission, RunId, RunMetadata, SearchSpec, SpecPatch, SymbolKey, SymbolKind,
    TaskBudget, TaskHandle, TaskKind, TaskState, WorkspaceState, WriteGrant, WriteScope,
};
use conseqa::spec::{
    Derivation, ExecuteTransaction, FieldPath, Id, IdempotencyGuarantee, IdempotencyKey,
    IdempotencyRequirement, Input, MessageSelector, ObjectSelector, OperationBlock, OperationStep,
    ResultReplayRequirement, Revision, SelectorPredicate, Service, ServiceKind, SubscriptionInput,
    Transaction, TransactionIsolation, TransactionSerializabilityRequirement, TransactionStep,
    ValueRef, ValueSource, Write,
};
use uuid::Uuid;

fn id(text: &str) -> Id {
    Id(text.to_string())
}

fn path(text: &str) -> FieldPath {
    FieldPath(text.split('.').map(str::to_string).collect())
}

fn fixture_workspace() -> WorkspaceState {
    let source =
        std::fs::read_to_string("tests/fixtures/flash_checkout.yaml").expect("fixture exists");

    let model = conseqa::parser::yaml::parse(&source).expect("fixture parses");

    WorkspaceState::from_model(&model, RunMetadata::new(RunId("occ-test".to_string())))
}

fn engine() -> ConfluenceEngine {
    ConfluenceEngine::in_memory(fixture_workspace()).expect("engine starts")
}

fn task(engine: &ConfluenceEngine, kind: TaskKind, scope: WriteScope) -> TaskHandle {
    engine
        .create_task(CreateTask {
            kind,
            objective: "test task".to_string(),
            write_scope: scope,
            prompt_evidence: Vec::new(),
            budget: TaskBudget::default(),
        })
        .expect("task is created")
}

fn submit(
    engine: &ConfluenceEngine,
    task: &TaskHandle,
    mutations: Vec<Mutation>,
) -> Result<CommitReceipt, CommitRejection> {
    engine
        .submit_blocking(CommitRequest {
            task: task.id,
            patch_id: PatchId::fresh(),
            base_revision: task.snapshot_revision,
            patch: SpecPatch { mutations },
            client_nonce: Uuid::new_v4(),
        })
        .expect("the sequencer is running")
}

/// A small, always-valid, distinguishable write to one operation's
/// program: a marker transaction that touches nothing, then a
/// terminal.
///
/// These tests are about the commit protocol — write-write conflicts,
/// read-set invalidation, scope enforcement — not about program
/// content. What they need of a mutation is only that it targets a
/// known operation-scoped symbol and that two different markers
/// produce different fingerprints.
fn probe_program(operation: &str, marker: u32) -> Mutation {
    let short = operation.strip_prefix("operation.").unwrap_or(operation);

    Mutation::ReplaceOperationProgram {
        operation: id(operation),
        program: OperationBlock {
            steps: vec![
                OperationStep::Transaction(ExecuteTransaction {
                    transaction: conseqa::spec::Transaction {
                        id: id(&format!("tx.{short}.probe{marker}")),
                        data_model: None,
                        isolation: conseqa::spec::TransactionIsolation::ReadCommitted,
                        idempotency: IdempotencyGuarantee::NotDeduplicated,
                        requirements: Default::default(),
                        steps: Vec::new(),
                    },
                    rejected: None,
                }),
                OperationStep::Complete,
            ],
        },
    }
}

/// Commits a patch through the current task in an interactive session's
/// token chain, using that task's live pinned revision as the base.
fn submit_session(engine: &ConfluenceEngine, token: &str, mutations: Vec<Mutation>) {
    let task = engine.resolve_token(token).expect("session token valid");

    let base_revision = engine
        .task_context(task)
        .expect("context")
        .snapshot_revision;

    engine
        .submit_blocking(CommitRequest {
            task,
            patch_id: PatchId::fresh(),
            base_revision,
            patch: SpecPatch { mutations },
            client_nonce: Uuid::new_v4(),
        })
        .expect("sequencer runs")
        .expect("the session commit is accepted");
}

fn canonical_schema(fields: &[(&str, &str)]) -> conseqa::spec::Schema {
    conseqa::spec::Schema::Canonical(conseqa::spec::CanonicalSchema {
        description: None,
        completeness: conseqa::spec::SchemaCompleteness::Complete,
        fields: fields
            .iter()
            .map(|(name, ty)| {
                (
                    name.to_string(),
                    conseqa::spec::Field {
                        ty: conseqa::spec::TypeRef::Scalar(match *ty {
                            "uuid" => conseqa::spec::ScalarType::Uuid,
                            _ => conseqa::spec::ScalarType::String,
                        }),
                        optional: false,
                    },
                )
            })
            .collect(),
    })
}

fn ping_interface() -> OperationInterfaceDraft {
    OperationInterfaceDraft {
        service: id("service.api"),
        description: Some("Echo the request id.".to_string()),
        inputs: BTreeMap::from([(
            id("input.ping.request"),
            Input::Request(conseqa::spec::RequestInput {
                schema: id("schema.PingRequest"),
                identity: conseqa::spec::RequestIdentity::Keyed(
                    conseqa::spec::RequestIdentityKey {
                        fields: vec![path("id")],
                    },
                ),
                result: conseqa::spec::ResultType {
                    ok: id("schema.PingResponse"),
                    errors: BTreeMap::from([(
                        id("rejected"),
                        conseqa::spec::ErrorResultType {
                            schema: id("schema.Rejected"),
                            disposition: conseqa::spec::ErrorDisposition::Terminal,
                        },
                    )]),
                },
            }),
        )]),
        sketch: None,
    }
}

fn ping_program() -> OperationBlock {
    OperationBlock {
        steps: vec![OperationStep::Return(conseqa::spec::Return {
            request: id("input.ping.request"),
            outcome: conseqa::spec::ResultOutcome::Ok {
                values: Derivation::Deterministic {
                    from: vec![ValueRef {
                        source: ValueSource::Input(id("input.ping.request")),
                        path: path("id"),
                    }],
                },
            },
        })],
    }
}

/// A program writing `object.order.status` directly — a new writer for
/// phantom tests. The order is versioned, so the write carries the
/// version bump the protocol requires.
fn status_writer_program() -> OperationBlock {
    OperationBlock {
        steps: vec![
            OperationStep::Transaction(ExecuteTransaction {
                transaction: Transaction {
                    id: id("tx.admin_force"),
                    data_model: Some(id("data.checkout")),
                    isolation: TransactionIsolation::ReadCommitted,
                    idempotency: IdempotencyGuarantee::Unspecified,
                    requirements: Default::default(),
                    steps: vec![
                        TransactionStep::Write(Write {
                            target: ObjectSelector {
                                object: id("object.order"),
                                predicate: SelectorPredicate::All,
                            },
                            fields: BTreeSet::from([path("status")]),
                            values: Derivation::Unspecified,
                        }),
                        TransactionStep::BumpVersion(conseqa::spec::BumpVersion {
                            target: ObjectSelector {
                                object: id("object.order"),
                                predicate: SelectorPredicate::All,
                            },
                        }),
                    ],
                },
                rejected: None,
            }),
            OperationStep::Complete,
        ],
    }
}

#[test]
fn interactive_session_commits_repeatedly_under_one_token() {
    let engine = engine();
    let mut events = engine.subscribe();

    let session = engine
        .create_session(WriteScope::shared_skeleton(), "interactive demo")
        .expect("session is created");

    let token = session.token.0.clone();
    let start = engine.head_revision().0;

    // Commit three services in a row through the one session token.
    let mut task_ids = Vec::new();

    for index in 0..3 {
        // The token resolves to the current task in the session's chain.
        let current = engine
            .resolve_token(&token)
            .expect("the session token stays valid");

        task_ids.push(current);

        // Each successive task is fresh and running, pinned to the head
        // the previous commit produced.
        let context = engine.task_context(current).expect("context");
        assert_eq!(context.state, TaskState::Running);
        assert_eq!(context.snapshot_revision.0, start + index);

        let receipt = engine
            .submit_blocking(CommitRequest {
                task: current,
                patch_id: PatchId::fresh(),
                base_revision: context.snapshot_revision,
                patch: SpecPatch {
                    mutations: vec![Mutation::PutService {
                        id: id(&format!("service.demo{index}")),
                        value: Service {
                            kind: ServiceKind::Backend,
                        },
                    }],
                },
                client_nonce: Uuid::new_v4(),
            })
            .expect("sequencer runs")
            .expect("the session commit is accepted");

        assert_eq!(receipt.revision.0, start + index + 1);
    }

    // Each commit rolled the token to a distinct successor task.
    assert_eq!(
        task_ids
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3,
        "each commit rolled to a fresh task"
    );

    // The head advanced once per commit, and all three services landed.
    assert_eq!(engine.head_revision().0, start + 3);

    let head = engine.head_snapshot();

    for index in 0..3 {
        assert!(
            head.workspace
                .services
                .contains_key(&id(&format!("service.demo{index}")))
        );
    }

    // The committed tasks emitted commit events; no invalidation of the
    // session ever occurred.
    let mut commits = 0;

    while let Ok(event) = events.try_recv() {
        match event {
            EngineEvent::TaskCommitted { .. } => commits += 1,
            EngineEvent::TaskInvalidated { task, .. } => {
                assert!(
                    !task_ids.contains(&task),
                    "the session was never invalidated"
                );
            }
            _ => {}
        }
    }

    assert_eq!(commits, 3);
}

#[test]
fn an_interactive_session_surfaces_the_run_prompt() {
    let mut run_meta = RunMetadata::new(RunId("prompted".to_string()));
    run_meta.prompt = Some("Build a URL shortener.".to_string());

    let engine = ConfluenceEngine::in_memory(WorkspaceState::empty(run_meta)).expect("engine");

    let session = engine
        .create_session(WriteScope::of([WriteGrant::All]), "build")
        .expect("session");

    let context = engine.task_context(session.id).expect("context");

    assert_eq!(context.prompt_evidence.len(), 1);
    assert_eq!(context.prompt_evidence[0].excerpt, "Build a URL shortener.");
}

#[test]
fn an_interactive_session_builds_a_new_project_from_empty() {
    // The new-project flow: an empty workspace, an interactive session
    // with the full authoring grant, building an architecture across
    // commits — including authoring an operation it created earlier in
    // the same session, which a per-existing-operation scope could not.
    let engine = ConfluenceEngine::in_memory(WorkspaceState::empty(RunMetadata::new(RunId(
        "new-project".to_string(),
    ))))
    .expect("engine starts");

    let session = engine
        .create_session(WriteScope::of([WriteGrant::All]), "build a system")
        .expect("session created");

    let token = session.token.0.clone();

    // Commit 1: the shared skeleton — a service, request/response
    // schemas, and one operation interface.
    let current = engine.resolve_token(&token).unwrap();

    submit_session(
        &engine,
        &token,
        vec![
            Mutation::PutService {
                id: id("service.api"),
                value: Service {
                    kind: ServiceKind::Backend,
                },
            },
            Mutation::PutSchema {
                id: id("schema.PingRequest"),
                value: canonical_schema(&[("id", "uuid")]),
            },
            Mutation::PutSchema {
                id: id("schema.PingResponse"),
                value: canonical_schema(&[("id", "uuid")]),
            },
            Mutation::PutSchema {
                id: id("schema.Rejected"),
                value: canonical_schema(&[("reason", "string")]),
            },
            Mutation::PutOperationInterface {
                operation: id("operation.ping"),
                value: ping_interface(),
            },
        ],
    );

    let _ = current;

    // Commit 2: author the program of the operation the session created
    // in commit 1. Requires the All grant — a per-existing-operation
    // scope, fixed at session creation, would not have covered
    // operation.ping.
    submit_session(
        &engine,
        &token,
        vec![Mutation::ReplaceOperationProgram {
            operation: id("operation.ping"),
            program: ping_program(),
        }],
    );

    assert_eq!(engine.head_revision().0, 2);

    let head = engine.head_snapshot();
    let draft = &head.workspace.operations[&id("operation.ping")];

    assert!(draft.program.is_some(), "the program was authored");
    assert!(draft.assemblable());
}

#[test]
fn non_conflicting_tasks_both_commit() {
    let engine = engine();

    let a = task(
        &engine,
        TaskKind::OperationSynthesis,
        WriteScope::operation_synthesis(id("operation.create_order")),
    );

    let b = task(
        &engine,
        TaskKind::OperationSynthesis,
        WriteScope::operation_synthesis(id("operation.transfer_stock")),
    );

    engine
        .read_symbol(a.id, &SymbolKey::Schema(id("schema.CreateOrderRequest")))
        .expect("a reads its schema");

    engine
        .read_symbol(b.id, &SymbolKey::Schema(id("schema.StockRecord")))
        .expect("b reads its schema");

    let first = submit(
        &engine,
        &a,
        vec![probe_program("operation.create_order", 2)],
    )
    .expect("a commits");

    let second = submit(
        &engine,
        &b,
        vec![probe_program("operation.transfer_stock", 3)],
    )
    .expect("b commits after a without conflict");

    assert_eq!(first.revision, Revision(2));
    assert_eq!(second.revision, Revision(3));
    assert_eq!(engine.head_revision(), Revision(3));
}

#[test]
fn read_write_conflict_invalidates_the_reader() {
    let engine = engine();
    let mut events = engine.subscribe();

    let a = task(
        &engine,
        TaskKind::OperationSynthesis,
        WriteScope::operation_synthesis(id("operation.transfer_stock")),
    );

    let b = task(
        &engine,
        TaskKind::OperationSynthesis,
        WriteScope::operation_synthesis(id("operation.create_order")),
    );

    // A reads what B is about to change.
    engine
        .read_symbol(
            a.id,
            &SymbolKey::OperationProgram(id("operation.create_order")),
        )
        .expect("a reads");

    submit(
        &engine,
        &b,
        vec![probe_program("operation.create_order", 2)],
    )
    .expect("b commits");

    // The commit invalidated A immediately.
    assert_eq!(engine.task_status(a.id).unwrap(), TaskState::Invalidated);

    let mut saw_invalidation = false;

    while let Ok(event) = events.try_recv() {
        if let EngineEvent::TaskInvalidated { task, causes, .. } = event {
            assert_eq!(task, a.id);

            assert!(matches!(
                causes.as_slice(),
                [InvalidationCause::ChangedSymbol { symbol }]
                    if *symbol == SymbolKey::OperationProgram(id("operation.create_order"))
            ));

            saw_invalidation = true;
        }
    }

    assert!(saw_invalidation, "invalidation event was published");

    // Invalidated tasks lose read tools...
    assert!(matches!(
        engine.read_symbol(a.id, &SymbolKey::Schema(id("schema.StockRecord"))),
        Err(EngineError::TaskInactive(TaskState::Invalidated)),
    ));

    // ...and cannot commit.
    let rejection = submit(
        &engine,
        &a,
        vec![probe_program("operation.transfer_stock", 2)],
    )
    .expect_err("a's commit is rejected");

    assert!(matches!(rejection, CommitRejection::TaskInvalidated));
    assert!(rejection.is_stale_context());

    // The replacement is a fresh task pinned to the current head.
    let replacement = task(
        &engine,
        TaskKind::OperationSynthesis,
        WriteScope::operation_synthesis(id("operation.transfer_stock")),
    );

    assert_ne!(replacement.id, a.id);
    assert_eq!(replacement.snapshot_revision, engine.head_revision());

    submit(
        &engine,
        &replacement,
        vec![probe_program("operation.transfer_stock", 2)],
    )
    .expect("the replacement commits cleanly");
}

#[test]
fn write_write_conflict_is_rejected() {
    let engine = engine();

    let a = task(
        &engine,
        TaskKind::OperationSynthesis,
        WriteScope::operation_synthesis(id("operation.create_order")),
    );

    let b = task(
        &engine,
        TaskKind::OperationSynthesis,
        WriteScope::operation_synthesis(id("operation.create_order")),
    );

    submit(
        &engine,
        &a,
        vec![probe_program("operation.create_order", 2)],
    )
    .expect("a commits first");

    // B read nothing, so it was not invalidated — but its write target
    // moved, and the gate compares write targets against the task's
    // base snapshot even when unread (§31).
    assert_eq!(engine.task_status(b.id).unwrap(), TaskState::Running);

    let rejection = submit(
        &engine,
        &b,
        vec![probe_program("operation.create_order", 9)],
    )
    .expect_err("b's replacement is rejected");

    assert!(matches!(
        rejection,
        CommitRejection::WriteConflict { symbol }
            if symbol == SymbolKey::OperationProgram(id("operation.create_order"))
    ));

    assert_eq!(engine.task_status(b.id).unwrap(), TaskState::Invalidated);
}

#[test]
fn phantom_new_writer_invalidates_the_querying_task() {
    let engine = engine();
    let mut events = engine.subscribe();

    let a = task(
        &engine,
        TaskKind::OperationSynthesis,
        WriteScope::operation_synthesis(id("operation.transfer_stock")),
    );

    // A asks who can write Order.status.
    engine
        .graph_query(
            a.id,
            &GraphQuery::Writers {
                data_model: id("data.checkout"),
                object: id("object.order"),
                field: Some(path("status")),
            },
        )
        .expect("a queries writers");

    // B introduces a new writer in one patch: interface + program.
    let b = task(
        &engine,
        TaskKind::Decompose,
        WriteScope::of([
            WriteGrant::SharedSkeleton,
            WriteGrant::OperationProgram(id("operation.admin_force_state")),
        ]),
    );

    for key in [
        SymbolKey::Service(id("service.checkout")),
        SymbolKey::DataModel(id("data.checkout")),
        SymbolKey::DataObject {
            data_model: id("data.checkout"),
            object: id("object.order"),
        },
    ] {
        engine.read_symbol(b.id, &key).expect("b reads its deps");
    }

    submit(
        &engine,
        &b,
        vec![
            Mutation::PutOperationInterface {
                operation: id("operation.admin_force_state"),
                value: OperationInterfaceDraft {
                    service: id("service.checkout"),
                    description: Some("Force an order state.".to_string()),
                    inputs: BTreeMap::new(),
                    sketch: None,
                },
            },
            Mutation::ReplaceOperationProgram {
                operation: id("operation.admin_force_state"),
                program: status_writer_program(),
            },
        ],
    )
    .expect("b commits the new writer");

    // No symbol A observed changed — but the answer to its set-valued
    // query did. That is the phantom.
    assert_eq!(engine.task_status(a.id).unwrap(), TaskState::Invalidated);

    let mut phantom_cause = false;

    while let Ok(event) = events.try_recv() {
        if let EngineEvent::TaskInvalidated { task, causes, .. } = event {
            assert_eq!(task, a.id);

            phantom_cause = causes.iter().any(|cause| {
                matches!(cause, InvalidationCause::ChangedQuery { query }
                    if matches!(query, GraphQuery::Writers { object, .. } if *object == id("object.order")))
            });
        }
    }

    assert!(phantom_cause, "the phantom was attributed to the query");

    let rejection = submit(
        &engine,
        &a,
        vec![probe_program("operation.transfer_stock", 2)],
    )
    .expect_err("a cannot commit on a stale answer");

    assert!(rejection.is_stale_context());
}

async fn submit_async(
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

/// A candidate is judged by the pipeline a committed revision meets —
/// scope, draft application, assembly, validation, verification — and
/// nothing else happens: no commit, no event, no task state change.
#[tokio::test]
async fn a_candidate_is_judged_without_being_committed() {
    let engine = engine();
    let mut events = engine.subscribe();

    let repairer = task(
        &engine,
        TaskKind::RequirementRepair,
        WriteScope::requirement_repair(id("operation.cancel_order")),
    );

    let before = engine.head_revision();

    // The empty candidate is the baseline: the model as it stands.
    let baseline = engine
        .evaluate_candidate(repairer.id, &SpecPatch::default(), &[])
        .await
        .expect("the task is active");

    assert!(baseline.refusal().is_none(), "{:?}", baseline.refusal());
    assert!(baseline.verified().is_some());

    // A candidate that replaces the program is verified as a whole
    // model, and the requirements its old program declared are no
    // longer declared — so they are not regressions.
    let candidate = SpecPatch {
        mutations: vec![probe_program("operation.cancel_order", 7)],
    };

    let verdict = engine
        .evaluate_candidate(repairer.id, &candidate, &[])
        .await
        .expect("the task is active");

    assert!(verdict.verified().is_some(), "{:?}", verdict.refusal());
    assert!(
        verdict
            .regressions(baseline.verified().expect("a baseline"))
            .is_empty()
    );

    // A candidate the scope does not authorize is judged no further.
    let outside = engine
        .evaluate_candidate(
            repairer.id,
            &SpecPatch {
                mutations: vec![probe_program("operation.transfer_stock", 7)],
            },
            &[],
        )
        .await
        .expect("the task is active");

    assert_eq!(
        outside.scope_violation,
        Some(SymbolKey::OperationProgram(id("operation.transfer_stock")))
    );
    assert!(outside.verified().is_none());

    assert_eq!(engine.head_revision(), before);
    assert_eq!(engine.task_status(repairer.id).unwrap(), TaskState::Running);

    // Background analysis of the opening head may report in meanwhile;
    // nothing a commit would publish does.
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(
                event,
                EngineEvent::HeadPublished { .. }
                    | EngineEvent::TaskCommitted { .. }
                    | EngineEvent::TaskInvalidated { .. }
            ),
            "evaluation published {event:?}"
        );
    }

    // The task can still commit what it evaluated.
    submit_async(&engine, &repairer, candidate.mutations)
        .await
        .expect("an evaluated candidate commits through the gate as any patch does");
}

/// What makes a partial repair safe: a candidate is compared with the
/// baseline requirement by requirement, and one that un-proves anything
/// — here, by dropping the lock a serializability proof rests on — says
/// so, wherever in the model the casualty is.
#[tokio::test]
async fn a_candidate_that_unproves_a_requirement_reports_the_regression() {
    let source =
        std::fs::read_to_string("tests/fixtures/tenant_ledger.yaml").expect("fixture exists");

    let model = conseqa::parser::yaml::parse(&source).expect("fixture parses");

    let engine = ConfluenceEngine::in_memory(WorkspaceState::from_model(
        &model,
        RunMetadata::new(RunId("regression-test".to_string())),
    ))
    .expect("engine starts");

    let repairer = task(
        &engine,
        TaskKind::RequirementRepair,
        WriteScope::requirement_repair(id("operation.post_entry")),
    );

    let roots = [(id("operation.post_entry"), id("tx.post_entry"))];

    let baseline = engine
        .evaluate_candidate(repairer.id, &SpecPatch::default(), &roots)
        .await
        .expect("the task is active");

    let baseline = baseline.verified().expect("the fixture verifies");

    assert!(baseline.all_proven());

    let mut program = model.operations[&id("operation.post_entry")]
        .program
        .clone();

    program
        .transaction_mut(&id("tx.post_entry"))
        .expect("the transaction")
        .steps
        .retain(|step| !matches!(step, TransactionStep::Lock(_)));

    let verdict = engine
        .evaluate_candidate(
            repairer.id,
            &SpecPatch {
                mutations: vec![Mutation::ReplaceOperationProgram {
                    operation: id("operation.post_entry"),
                    program,
                }],
            },
            &roots,
        )
        .await
        .expect("the task is active");

    let regressions = verdict.regressions(baseline);

    assert_eq!(regressions.len(), 1, "{regressions:?}");

    assert_eq!(
        regressions[0].to_string(),
        "transaction_serializability #0 of tx.post_entry in operation.post_entry"
    );
}

/// A verdict is a fact its caller relies on, so what it rests on is
/// observed. Naming a transaction as a root records who reads and
/// writes every object its conflict closure accesses: a transaction
/// that joins the closure afterwards is a phantom, and the evaluator is
/// invalidated rather than left to submit on a stale proof.
#[tokio::test]
async fn a_transaction_joining_an_evaluated_closure_invalidates_the_evaluator() {
    let engine = engine();

    let rooted = task(
        &engine,
        TaskKind::RequirementRepair,
        WriteScope::requirement_repair(id("operation.cancel_order")),
    );

    let unrooted = task(
        &engine,
        TaskKind::RequirementRepair,
        WriteScope::requirement_repair(id("operation.charge_payment")),
    );

    engine
        .evaluate_candidate(
            rooted.id,
            &SpecPatch::default(),
            &[(id("operation.cancel_order"), id("tx.cancel_order"))],
        )
        .await
        .expect("the task is active");

    // The same evaluation with no root claims nothing about any
    // closure, and observes none.
    engine
        .evaluate_candidate(unrooted.id, &SpecPatch::default(), &[])
        .await
        .expect("the task is active");

    // A new writer of `object.order` arrives.
    let author = task(
        &engine,
        TaskKind::Decompose,
        WriteScope::of([
            WriteGrant::SharedSkeleton,
            WriteGrant::OperationProgram(id("operation.admin_force_state")),
        ]),
    );

    for key in [
        SymbolKey::Service(id("service.checkout")),
        SymbolKey::DataModel(id("data.checkout")),
        SymbolKey::DataObject {
            data_model: id("data.checkout"),
            object: id("object.order"),
        },
    ] {
        engine
            .read_symbol(author.id, &key)
            .expect("the author reads its deps");
    }

    submit_async(
        &engine,
        &author,
        vec![
            Mutation::PutOperationInterface {
                operation: id("operation.admin_force_state"),
                value: OperationInterfaceDraft {
                    service: id("service.checkout"),
                    description: Some("Force an order state.".to_string()),
                    inputs: BTreeMap::new(),
                    sketch: None,
                },
            },
            Mutation::ReplaceOperationProgram {
                operation: id("operation.admin_force_state"),
                program: status_writer_program(),
            },
        ],
    )
    .await
    .expect("the new writer commits");

    assert_eq!(
        engine.task_status(rooted.id).unwrap(),
        TaskState::Invalidated
    );

    assert_eq!(engine.task_status(unrooted.id).unwrap(), TaskState::Running);
}

/// A transaction requirement is adopted onto the inline transaction it
/// constrains, so proposing one changes the program's fingerprint. The
/// proposal is *authorized* by a requirements grant and *conflicts* as
/// a program write: a proposer whose base predates a replacement of
/// that program is refused, though it never read the program and its
/// own write target did not move.
#[test]
fn a_transaction_requirement_proposal_conflicts_with_a_program_replacement() {
    let engine = engine();

    let proposer = task(
        &engine,
        TaskKind::RequirementDiscovery,
        WriteScope::requirement_discovery(id("operation.cancel_order")),
    );

    let synthesizer = task(
        &engine,
        TaskKind::OperationSynthesis,
        WriteScope::operation_synthesis(id("operation.cancel_order")),
    );

    submit(
        &engine,
        &synthesizer,
        vec![probe_program("operation.cancel_order", 1)],
    )
    .expect("the program is replaced");

    let proposal = |transaction: &str| Mutation::ProposeRequirements {
        operation: id("operation.cancel_order"),
        proposals: vec![RequirementSubmission {
            requirement: ProposedRequirement::TransactionSerializability {
                transaction: id(transaction),
                requirement: TransactionSerializabilityRequirement {
                    key: ValueRef {
                        source: ValueSource::Input(id("input.cancel_order.request")),
                        path: path("order_id"),
                    },
                },
            },
            origin: RequirementOrigin::Recommended {
                rationale: "cancellations of one order must not interleave".to_string(),
                evidence: Vec::new(),
            },
        }],
    };

    // The footprint says so before anything is submitted.
    assert_eq!(
        SpecPatch {
            mutations: vec![proposal("tx.cancel_order")],
        }
        .write_targets(),
        vec![
            SymbolKey::OperationProgram(id("operation.cancel_order")),
            SymbolKey::OperationRequirements(id("operation.cancel_order")),
        ]
    );

    let rejection = submit(&engine, &proposer, vec![proposal("tx.cancel_order")])
        .expect_err("the program moved under the proposal");

    assert!(
        matches!(
            &rejection,
            CommitRejection::WriteConflict { symbol }
                if *symbol == SymbolKey::OperationProgram(id("operation.cancel_order"))
        ),
        "{rejection:?}"
    );

    assert!(rejection.is_stale_context());

    // Authorization did not widen: the same grant still cannot replace
    // the program.
    let discoverer = task(
        &engine,
        TaskKind::RequirementDiscovery,
        WriteScope::requirement_discovery(id("operation.cancel_order")),
    );

    let rejection = submit(
        &engine,
        &discoverer,
        vec![probe_program("operation.cancel_order", 2)],
    )
    .expect_err("a requirements grant replaces no program");

    assert!(
        matches!(rejection, CommitRejection::WriteScopeViolation { .. }),
        "{rejection:?}"
    );

    // An operation-family proposal still touches the requirements alone.
    assert_eq!(
        SpecPatch {
            mutations: vec![Mutation::ProposeRequirements {
                operation: id("operation.cancel_order"),
                proposals: vec![RequirementSubmission {
                    requirement: ProposedRequirement::Idempotency(IdempotencyRequirement {
                        key: IdempotencyKey {
                            components: vec![ValueRef {
                                source: ValueSource::Input(id("input.cancel_order.request")),
                                path: path("request_id"),
                            }],
                        },
                        result: ResultReplayRequirement::Unspecified,
                    }),
                    origin: RequirementOrigin::Recommended {
                        rationale: "retries".to_string(),
                        evidence: Vec::new(),
                    },
                }],
            }],
        }
        .write_targets(),
        vec![SymbolKey::OperationRequirements(id(
            "operation.cancel_order"
        ))]
    );
}

/// Mapping a prompt obligation rewrites it, and discovery fans out one
/// task per operation. A search narrowed to the obligations *targeting*
/// one operation observes only that set: a peer rewriting another
/// operation's obligation leaves the searcher valid, while an obligation
/// newly aimed at its operation — a phantom — does not.
#[test]
fn a_targeted_obligation_search_observes_only_its_own_operations_obligations() {
    let engine = engine();

    let obligation = |intent: &str, targets: &[&str]| PromptObligation {
        source_span: None,
        normalized_intent: intent.to_string(),
        targets: targets.iter().map(|target| id(target)).collect(),
        status: PromptObligationStatus::Unmapped,
    };

    let put = |name: &str, value: PromptObligation| {
        let author = task(
            &engine,
            TaskKind::Decompose,
            WriteScope::of([WriteGrant::SharedSkeleton]),
        );

        submit(
            &engine,
            &author,
            vec![Mutation::PutPromptObligation {
                id: PromptObligationId(name.to_string()),
                value,
            }],
        )
        .expect("the obligation is recorded");
    };

    put(
        "obl.order-once",
        obligation("an order is created once", &["operation.create_order"]),
    );

    put(
        "obl.stock-in-order",
        obligation("stock moves apply in order", &["operation.transfer_stock"]),
    );

    let searcher = task(
        &engine,
        TaskKind::RequirementDiscovery,
        WriteScope::requirement_discovery(id("operation.create_order")),
    );

    let aimed_here = engine
        .search_symbols(
            searcher.id,
            &SearchSpec {
                kind: Some(SymbolKind::PromptObligation),
                targets: Some(id("operation.create_order")),
                ..Default::default()
            },
        )
        .expect("the search runs");

    assert_eq!(
        aimed_here,
        vec![SymbolKey::PromptObligation(PromptObligationId(
            "obl.order-once".to_string()
        ))]
    );

    // A peer rewrites the obligation aimed elsewhere — as mapping it
    // would. The searcher observed neither it nor a set containing it.
    put(
        "obl.stock-in-order",
        obligation(
            "stock moves apply in sequence order",
            &["operation.transfer_stock"],
        ),
    );

    assert_eq!(engine.task_status(searcher.id).unwrap(), TaskState::Running);

    // The same obligation is now aimed here as well: the answer to the
    // searcher's set-valued question changed.
    put(
        "obl.stock-in-order",
        obligation(
            "stock moves apply in sequence order",
            &["operation.transfer_stock", "operation.create_order"],
        ),
    );

    assert_eq!(
        engine.task_status(searcher.id).unwrap(),
        TaskState::Invalidated
    );
}

#[test]
fn removed_dependency_invalidates_the_reader() {
    let engine = engine();
    let mut events = engine.subscribe();

    let a = task(
        &engine,
        TaskKind::OperationSynthesis,
        WriteScope::operation_synthesis(id("operation.transfer_stock")),
    );

    engine
        .read_symbol(a.id, &SymbolKey::Schema(id("schema.OrderPaid")))
        .expect("a reads the schema");

    let coordinator = task(
        &engine,
        TaskKind::SharedDependencyRepair,
        WriteScope::of([WriteGrant::TopLevelSymbol(SymbolKey::Schema(id(
            "schema.OrderPaid",
        )))]),
    );

    submit(
        &engine,
        &coordinator,
        vec![Mutation::DeleteTopLevel {
            symbol: SymbolKey::Schema(id("schema.OrderPaid")),
        }],
    )
    .expect("the coordinator deletes the schema");

    assert_eq!(engine.task_status(a.id).unwrap(), TaskState::Invalidated);

    let mut removed_cause = false;

    while let Ok(event) = events.try_recv() {
        if let EngineEvent::TaskInvalidated { task, causes, .. } = event
            && task == a.id
        {
            removed_cause = causes.iter().any(|cause| {
                matches!(cause, InvalidationCause::RemovedSymbol { symbol }
                    if *symbol == SymbolKey::Schema(id("schema.OrderPaid")))
            });
        }
    }

    assert!(removed_cause, "the removal was the recorded cause");
}

#[test]
fn unobserved_reference_is_rejected_then_fixable() {
    let engine = engine();

    let operation = id("operation.gateway");

    let a = task(
        &engine,
        TaskKind::OperationSynthesis,
        WriteScope::of([
            WriteGrant::SharedSkeleton,
            WriteGrant::OperationProgram(operation.clone()),
        ]),
    );

    // A reads the service and the request schema — but not the callee
    // interface its program is about to reference.
    engine
        .read_symbol(a.id, &SymbolKey::Service(id("service.checkout")))
        .expect("a reads the service");

    engine
        .read_symbol(a.id, &SymbolKey::Schema(id("schema.CreateOrderRequest")))
        .expect("a reads the schema");

    let mutations = vec![
        Mutation::PutOperationInterface {
            operation: operation.clone(),
            value: OperationInterfaceDraft {
                service: id("service.checkout"),
                description: Some("Calls create_order.".to_string()),
                inputs: BTreeMap::new(),
                sketch: None,
            },
        },
        Mutation::ReplaceOperationProgram {
            operation: operation.clone(),
            program: OperationBlock {
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
                        values: Derivation::Unspecified,
                        bind: None,
                    }),
                    OperationStep::Complete,
                ],
            },
        },
    ];

    let rejection = submit(&engine, &a, mutations.clone()).expect_err("the reference is unread");

    assert!(matches!(
        &rejection,
        CommitRejection::UnobservedDependency { symbol }
            if *symbol == SymbolKey::OperationInterface(id("operation.create_order"))
    ));

    assert!(!rejection.is_stale_context(), "fixable in-session");
    assert_eq!(engine.task_status(a.id).unwrap(), TaskState::Running);

    // Reading the callee interface fixes it.
    engine
        .read_operation(
            a.id,
            &id("operation.create_order"),
            OperationReadMode::Interface,
        )
        .expect("a reads the callee interface");

    submit(&engine, &a, mutations).expect("the resubmission commits");
}

#[test]
fn duplicate_submission_commits_once() {
    let engine = engine();

    let a = task(
        &engine,
        TaskKind::OperationSynthesis,
        WriteScope::operation_synthesis(id("operation.create_order")),
    );

    let request = CommitRequest {
        task: a.id,
        patch_id: PatchId::fresh(),
        base_revision: a.snapshot_revision,
        patch: SpecPatch {
            mutations: vec![probe_program("operation.create_order", 2)],
        },
        client_nonce: Uuid::new_v4(),
    };

    let first = engine
        .submit_blocking(request.clone())
        .expect("sequencer up")
        .expect("first submission commits");

    let second = engine
        .submit_blocking(request)
        .expect("sequencer up")
        .expect("second submission replays");

    assert!(!first.replayed);
    assert!(second.replayed);
    assert_eq!(first.revision, second.revision);
    assert_eq!(engine.head_revision(), first.revision);
}

#[test]
fn stale_read_after_head_moved_is_caught_at_commit() {
    let engine = engine();

    let c = task(
        &engine,
        TaskKind::OperationSynthesis,
        WriteScope::operation_synthesis(id("operation.transfer_stock")),
    );

    let d = task(
        &engine,
        TaskKind::OperationSynthesis,
        WriteScope::operation_synthesis(id("operation.create_order")),
    );

    // D commits before C reads anything, so the sweep leaves C alone.
    submit(
        &engine,
        &d,
        vec![probe_program("operation.create_order", 2)],
    )
    .expect("d commits");

    assert_eq!(engine.task_status(c.id).unwrap(), TaskState::Running);

    // C now reads the changed symbol — from its own pinned snapshot,
    // observing the stale fingerprint.
    engine
        .read_symbol(
            c.id,
            &SymbolKey::OperationProgram(id("operation.create_order")),
        )
        .expect("c reads from its pinned snapshot");

    let rejection = submit(
        &engine,
        &c,
        vec![probe_program("operation.transfer_stock", 2)],
    )
    .expect_err("the stale observation is caught at the gate");

    assert!(matches!(
        rejection,
        CommitRejection::ReadConflict { symbol, .. }
            if symbol == SymbolKey::OperationProgram(id("operation.create_order"))
    ));

    assert_eq!(engine.task_status(c.id).unwrap(), TaskState::Invalidated);
}

#[test]
fn wrong_base_revision_is_rejected() {
    let engine = engine();

    let a = task(
        &engine,
        TaskKind::OperationSynthesis,
        WriteScope::operation_synthesis(id("operation.create_order")),
    );

    let rejection = engine
        .submit_blocking(CommitRequest {
            task: a.id,
            patch_id: PatchId::fresh(),
            base_revision: Revision(999),
            patch: SpecPatch {
                mutations: vec![probe_program("operation.create_order", 2)],
            },
            client_nonce: Uuid::new_v4(),
        })
        .expect("sequencer up")
        .expect_err("mismatched base is rejected");

    assert!(matches!(
        rejection,
        CommitRejection::BaseRevisionMismatch { .. }
    ));
}

#[test]
fn write_scope_violation_is_rejected_and_fixable() {
    let engine = engine();

    let a = task(
        &engine,
        TaskKind::OperationSynthesis,
        WriteScope::operation_synthesis(id("operation.create_order")),
    );

    let rejection = submit(
        &engine,
        &a,
        vec![probe_program("operation.transfer_stock", 2)],
    )
    .expect_err("out-of-scope write is rejected");

    assert!(matches!(
        rejection,
        CommitRejection::WriteScopeViolation { attempted }
            if attempted == SymbolKey::OperationProgram(id("operation.transfer_stock"))
    ));

    assert_eq!(engine.task_status(a.id).unwrap(), TaskState::Running);
}

/// The L1 runtime topology has exactly one holder. Every other scope
/// in the workflow — decomposition included — is refused, so a pool and
/// the router that terminates at it can never be written by two
/// concurrent workers with different pictures of the system.
#[test]
fn only_the_topology_scope_may_write_the_runtime_model() {
    let engine = engine();

    let pool = || Mutation::PutExecutionPool {
        id: id("pool.probe"),
        value: conseqa::spec::ExecutionPool {
            member_concurrency: conseqa::spec::MemberConcurrency::Bounded(
                std::num::NonZeroU32::new(1).expect("non-zero"),
            ),
        },
    };

    for scope in [
        WriteScope::shared_skeleton(),
        WriteScope::operation_synthesis(id("operation.create_order")),
        WriteScope::requirement_repair(id("operation.create_order")),
        WriteScope::requirement_discovery(id("operation.create_order")),
    ] {
        let denied = task(&engine, TaskKind::OperationSynthesis, scope.clone());

        let rejection = submit(&engine, &denied, vec![pool()])
            .expect_err("an L1 write outside the topology scope is rejected");

        assert!(
            matches!(
                rejection,
                CommitRejection::WriteScopeViolation { ref attempted }
                    if *attempted == SymbolKey::ExecutionPool(id("pool.probe"))
            ),
            "{scope:?} accepted an L1 write: {rejection:?}"
        );
    }

    let author = task(
        &engine,
        TaskKind::TopologySynthesis,
        WriteScope::runtime_topology(),
    );

    submit(&engine, &author, vec![pool()]).expect("the topology scope may write L1");
}

#[test]
fn draft_validation_failure_is_precise_and_fixable() {
    let engine = engine();

    let a = task(
        &engine,
        TaskKind::OperationSynthesis,
        WriteScope::operation_synthesis(id("operation.charge_payment")),
    );

    engine
        .read_symbol(a.id, &SymbolKey::Schema(id("schema.ChargeAccepted")))
        .expect("a reads the output schema");

    // A program referencing an undeclared input.
    let rejection = submit(
        &engine,
        &a,
        vec![Mutation::ReplaceOperationProgram {
            operation: id("operation.charge_payment"),
            program: OperationBlock {
                steps: vec![
                    OperationStep::Transaction(ExecuteTransaction {
                        transaction: Transaction {
                            id: id("tx.bogus"),
                            data_model: None,
                            isolation: TransactionIsolation::Unspecified,
                            idempotency: IdempotencyGuarantee::Unspecified,
                            requirements: Default::default(),
                            steps: vec![TransactionStep::EstablishTransactionOutput(
                                conseqa::spec::EstablishTransactionOutput {
                                    bind: id("output.bogus"),
                                    schema: id("schema.ChargeAccepted"),
                                    values: Derivation::Deterministic {
                                        from: vec![ValueRef {
                                            source: ValueSource::Input(id("input.does_not_exist")),
                                            path: path("event_id"),
                                        }],
                                    },
                                },
                            )],
                        },
                        rejected: None,
                    }),
                    OperationStep::Complete,
                ],
            },
        }],
    )
    .expect_err("the undeclared input is diagnosed");

    let CommitRejection::DraftValidationFailed { diagnostics } = &rejection else {
        panic!("expected draft validation failure, got {rejection:?}");
    };

    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("input.does_not_exist")),
        "{diagnostics:?}"
    );

    assert!(!rejection.is_stale_context());
    assert_eq!(engine.task_status(a.id).unwrap(), TaskState::Running);
}

/// The gate judges a transaction's requirement keys structurally, so a
/// broken declaration is fixed in the same session: a serializability
/// or ordering key must source an input the operation's interface
/// declares.
#[test]
fn the_gate_refuses_a_transaction_requirement_keyed_from_an_undeclared_input() {
    let engine = engine();

    let decomposer = task(&engine, TaskKind::Decompose, WriteScope::shared_skeleton());

    for key in [
        SymbolKey::Service(id("service.checkout")),
        SymbolKey::Topic(id("topic.order_events")),
        SymbolKey::Schema(id("schema.OrderPaid")),
    ] {
        engine
            .read_symbol(decomposer.id, &key)
            .expect("the decomposer reads what it references");
    }

    submit(
        &engine,
        &decomposer,
        vec![Mutation::PutOperationInterface {
            operation: id("operation.notify"),
            value: OperationInterfaceDraft {
                service: id("service.checkout"),
                description: Some("Notify on payment.".to_string()),
                inputs: BTreeMap::from([(
                    id("input.notify.paid"),
                    Input::Subscription(SubscriptionInput {
                        topic: id("topic.order_events"),
                        messages: MessageSelector::Only(BTreeSet::from([id("schema.OrderPaid")])),
                        acknowledge_on_success: None,
                    }),
                )]),
                sketch: None,
            },
        }],
    )
    .expect("the interface commits");

    let synthesizer = task(
        &engine,
        TaskKind::OperationSynthesis,
        WriteScope::operation_synthesis(id("operation.notify")),
    );

    let program = |input: &str| Mutation::ReplaceOperationProgram {
        operation: id("operation.notify"),
        program: OperationBlock {
            steps: vec![
                OperationStep::Transaction(ExecuteTransaction {
                    transaction: Transaction {
                        id: id("tx.notify"),
                        data_model: None,
                        isolation: TransactionIsolation::Serializable,
                        idempotency: IdempotencyGuarantee::Unspecified,
                        requirements: conseqa::spec::TransactionRequirements {
                            serializability: vec![TransactionSerializabilityRequirement {
                                key: ValueRef {
                                    source: ValueSource::Input(id(input)),
                                    path: path("order_id"),
                                },
                            }],
                            ordering: Vec::new(),
                        },
                        steps: Vec::new(),
                    },
                    rejected: None,
                }),
                OperationStep::Complete,
            ],
        },
    };

    // Keyed on an input the interface does not declare.
    let rejection = submit(
        &engine,
        &synthesizer,
        vec![program("input.notify.undeclared")],
    )
    .expect_err("an undeclared key input is refused");

    let CommitRejection::DraftValidationFailed { diagnostics } = &rejection else {
        panic!("expected draft validation failure, got {rejection:?}");
    };

    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("input.notify.undeclared")),
        "{diagnostics:?}"
    );

    assert!(!rejection.is_stale_context());

    // Keyed on the interface's input: commits, and the requirement
    // lands on the transaction.
    submit(&engine, &synthesizer, vec![program("input.notify.paid")])
        .expect("a coherent requirement commits");

    let head = engine.head_snapshot();
    let draft = &head.workspace.operations[&id("operation.notify")];

    let transaction = draft
        .program
        .as_ref()
        .and_then(|program| program.transaction(&id("tx.notify")))
        .expect("the program declares the transaction");

    assert_eq!(transaction.requirements.serializability.len(), 1);
}

#[test]
fn planned_operation_commits_as_draft_and_requirements_flow_through_proposals() {
    let engine = engine();

    // The decomposer plans a new operation and records an explicit
    // prompt obligation, in one skeleton patch.
    let decomposer = task(&engine, TaskKind::Decompose, WriteScope::shared_skeleton());

    for key in [
        SymbolKey::Service(id("service.checkout")),
        SymbolKey::Topic(id("topic.order_events")),
        SymbolKey::Schema(id("schema.OrderPaid")),
    ] {
        engine
            .read_symbol(decomposer.id, &key)
            .expect("the decomposer reads what it references");
    }

    let obligation = PromptObligationId("obl.notify-once".to_string());

    submit(
        &engine,
        &decomposer,
        vec![
            Mutation::PutOperationInterface {
                operation: id("operation.notify"),
                value: OperationInterfaceDraft {
                    service: id("service.checkout"),
                    description: Some("Notify on payment.".to_string()),
                    inputs: BTreeMap::from([(
                        id("input.notify.paid"),
                        Input::Subscription(SubscriptionInput {
                            topic: id("topic.order_events"),
                            messages: MessageSelector::Only(BTreeSet::from([id(
                                "schema.OrderPaid",
                            )])),
                            acknowledge_on_success: None,
                        }),
                    )]),
                    sketch: None,
                },
            },
            Mutation::PutPromptObligation {
                id: obligation.clone(),
                value: PromptObligation {
                    source_span: Some("notifications must go out exactly once".to_string()),
                    normalized_intent: "notify exactly once per payment".to_string(),
                    targets: vec![id("operation.notify")],
                    status: PromptObligationStatus::Unmapped,
                },
            },
        ],
    )
    .expect("the skeleton patch commits although the operation has no program");

    // The draft head holds a planned, unassemblable operation (§102).
    let head = engine.head_snapshot();
    let draft = &head.workspace.operations[&id("operation.notify")];

    assert_eq!(draft.stage, OperationDraftStage::Planned);
    assert!(!draft.assemblable());

    // Requirement discovery proposes; the gate records and adopts.
    let discovery = task(
        &engine,
        TaskKind::RequirementDiscovery,
        WriteScope::requirement_discovery(id("operation.notify")),
    );

    submit(
        &engine,
        &discovery,
        vec![Mutation::ProposeRequirements {
            operation: id("operation.notify"),
            proposals: vec![RequirementSubmission {
                requirement: ProposedRequirement::Idempotency(IdempotencyRequirement {
                    key: IdempotencyKey {
                        components: vec![ValueRef {
                            source: ValueSource::Input(id("input.notify.paid")),
                            path: path("event_id"),
                        }],
                    },
                    result: ResultReplayRequirement::Unspecified,
                }),
                origin: RequirementOrigin::ExplicitPrompt {
                    obligation: obligation.clone(),
                },
            }],
        }],
    )
    .expect("the proposal commits");

    let head = engine.head_snapshot();
    let draft = &head.workspace.operations[&id("operation.notify")];

    assert_eq!(draft.requirements.idempotency.len(), 1);

    let proposal = &head.workspace.requirement_proposals[0];

    assert!(
        matches!(&proposal.status, ProposalStatus::Adopted { reference }
        if reference.operation == id("operation.notify") && reference.index == 0)
    );

    assert!(matches!(
        &head.workspace.prompt_obligations[&obligation].status,
        PromptObligationStatus::Mapped { requirements } if requirements.len() == 1
    ));
}

#[test]
fn recovery_restores_the_head_and_invalidates_active_tasks() {
    let dir = std::env::temp_dir().join(format!("conseqa-occ-{}", Uuid::new_v4()));
    let database = dir.join("confluence.redb");

    let committed_revision;
    let orphaned_task;

    {
        let engine = ConfluenceEngine::open(&database, fixture_workspace()).expect("engine opens");

        let a = task(
            &engine,
            TaskKind::OperationSynthesis,
            WriteScope::operation_synthesis(id("operation.create_order")),
        );

        committed_revision = submit(
            &engine,
            &a,
            vec![probe_program("operation.create_order", 2)],
        )
        .expect("the commit persists")
        .revision;

        orphaned_task = task(
            &engine,
            TaskKind::OperationSynthesis,
            WriteScope::operation_synthesis(id("operation.transfer_stock")),
        )
        .id;
    }

    // The old engine (and its sequencer thread) shut down when every
    // handle dropped; redb releases its lock with it. Retry briefly.
    let reopened = {
        let mut attempt = 0;

        loop {
            match ConfluenceEngine::open(&database, fixture_workspace()) {
                Ok(engine) => break engine,
                Err(_) if attempt < 50 => {
                    attempt += 1;

                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(error) => panic!("reopen failed: {error}"),
            }
        }
    };

    assert_eq!(reopened.head_revision(), committed_revision);

    // The workspace content survived.
    let head = reopened.head_snapshot();

    assert_eq!(
        head.workspace.operations[&id("operation.create_order")].program,
        Some(match probe_program("operation.create_order", 2) {
            Mutation::ReplaceOperationProgram { program, .. } => program,
            _ => unreachable!("probe_program builds a program mutation"),
        }),
    );

    // The task that was running when the process died is conservatively
    // invalidated: its live read tracking is gone (§84).
    assert_eq!(
        reopened.task_status(orphaned_task).unwrap(),
        TaskState::Invalidated
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// An invalidated task leaves a note for its replacement: the causes,
/// and — when the attempt got as far as submitting — the rejected
/// patch, so the next session starts from review-and-resubmit instead
/// of re-deriving the work (a warm restart).
#[test]
fn invalidation_notes_carry_causes_and_the_salvageable_patch() {
    let engine = engine();

    // Gate-side invalidation: the task read nothing that changed, so
    // it survives the sweep and learns of the conflict only when its
    // own submission hits the gate — with the patch in hand.
    let a = task(
        &engine,
        TaskKind::OperationSynthesis,
        WriteScope::operation_synthesis(id("operation.create_order")),
    );

    assert!(
        engine.invalidation_note(a.id).is_none(),
        "a running task has no note"
    );

    let interloper = task(
        &engine,
        TaskKind::Decompose,
        WriteScope::of([WriteGrant::All]),
    );

    submit(
        &engine,
        &interloper,
        vec![probe_program("operation.create_order", 1)],
    )
    .expect("the interloper commits");

    let rejection = submit(
        &engine,
        &a,
        vec![probe_program("operation.create_order", 2)],
    )
    .expect_err("the write target moved under the task");

    assert!(rejection.is_stale_context(), "{rejection:?}");
    assert_eq!(engine.task_status(a.id).unwrap(), TaskState::Invalidated);

    let note = engine
        .invalidation_note(a.id)
        .expect("a gate-invalidated task leaves a note");

    assert!(
        note.causes.iter().any(|cause| matches!(
            cause,
            InvalidationCause::ChangedSymbol { symbol }
                if *symbol == SymbolKey::OperationProgram(id("operation.create_order"))
        )),
        "{:?}",
        note.causes
    );

    let patch = note.rejected_patch.expect("the refused patch is salvaged");

    assert_eq!(patch.mutations.len(), 1);

    // Sweep-side invalidation: a task cancelled mid-reasoning has no
    // patch to salvage, but the causes still travel.
    let c = task(
        &engine,
        TaskKind::OperationSynthesis,
        WriteScope::operation_synthesis(id("operation.create_order")),
    );

    engine
        .read_symbol(
            c.id,
            &SymbolKey::OperationProgram(id("operation.transfer_stock")),
        )
        .expect("c observes a peer's program");

    let interloper = task(
        &engine,
        TaskKind::Decompose,
        WriteScope::of([WriteGrant::All]),
    );

    submit(
        &engine,
        &interloper,
        vec![probe_program("operation.transfer_stock", 3)],
    )
    .expect("the second interloper commits");

    assert_eq!(engine.task_status(c.id).unwrap(), TaskState::Invalidated);

    let note = engine
        .invalidation_note(c.id)
        .expect("a swept task leaves a note");

    assert!(note.rejected_patch.is_none(), "nothing was submitted");

    assert!(
        note.causes.iter().any(|cause| matches!(
            cause,
            InvalidationCause::ChangedSymbol { symbol }
                if *symbol == SymbolKey::OperationProgram(id("operation.transfer_stock"))
        )),
        "{:?}",
        note.causes
    );
}
