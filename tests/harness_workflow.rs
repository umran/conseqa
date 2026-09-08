//! Full design-workflow test (§104, §107 phase 7 of the confluence
//! spec): prompt to validated Conseqa YAML with an all-adopted,
//! all-proven requirement report — driven by a scripted in-process
//! backend that performs real commits through the confluence engine,
//! so the phase machine, OCC gate, analysis, and finalization are all
//! exercised without a live coding agent.
//!
//! The scripted backend is a legitimate test double: it resolves its
//! task capability exactly as a real agent would, reads shared symbols
//! to satisfy read-before-reference, and submits typed patches through
//! the same gate. Only the LLM reasoning is replaced by a fixed plan.

#![allow(clippy::result_large_err)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use conseqa::confluence::{
    AgentBackendMetadata, CommitRequest, ConfluenceEngine, Mutation, OperationInterfaceDraft,
    PatchId, ProposedRequirement, PromptObligation, PromptObligationId, PromptObligationStatus,
    RequirementOrigin, RequirementSubmission, RunId, RunMetadata, RunPolicy, SpecPatch,
    WorkspaceState,
};
use conseqa::harness::backend::{
    AgentBackend, AgentBackendError, AgentEventSink, AgentExit, AgentExitStatus, AgentHandle,
    AgentInvocation,
};
use conseqa::harness::{
    RunStatus, Scheduler, SchedulerPolicy, Supervisor, Workflow, WorkflowConfig,
};
use conseqa::spec::{
    CanonicalSchema, Derivation, ErrorDisposition, ErrorResultType, Field,
    Id, IdempotencyKey, Input, OperationBlock, OperationStep,
    RequestIdentity, RequestInput, ResultOutcome, ResultType, Return, ScalarType, Schema,
    SchemaCompleteness, SerializationRequirement, Service, ServiceKind, TypeRef, ValueRef,
    ValueSource,
};
use uuid::Uuid;

fn id(text: &str) -> Id {
    Id(text.to_string())
}

fn path(text: &str) -> conseqa::spec::FieldPath {
    conseqa::spec::FieldPath(text.split('.').map(str::to_string).collect())
}

/// A future the scripted backend runs to perform one task's commits.
type ScriptFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// The scripted plan: given the engine and the invocation (whose kind
/// selects the behavior), perform the task's commits.
type ScriptFn = Arc<dyn Fn(ConfluenceEngine, AgentInvocation) -> ScriptFuture + Send + Sync>;

/// An in-process backend that executes a fixed plan instead of
/// launching a coding agent. It commits through the real engine, so
/// the OCC gate, analysis, and invalidation all behave normally.
#[derive(Clone)]
struct ScriptedBackend {
    engine: ConfluenceEngine,
    script: ScriptFn,
}

#[async_trait]
impl AgentBackend for ScriptedBackend {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn run(
        &self,
        invocation: AgentInvocation,
        _handle: AgentHandle,
        _events: AgentEventSink,
    ) -> Result<AgentExit, AgentBackendError> {
        // Resolve the capability exactly as a real agent's MCP request
        // would, proving the token plumbing end to end.
        let session = self
            .engine
            .resolve_token(&invocation.task_token)
            .map(|task| task.to_string());

        (self.script)(self.engine.clone(), invocation).await;

        Ok(AgentExit {
            status: AgentExitStatus::Completed,
            session: session.clone(),
            final_message: Some("scripted task done".to_string()),
            usage: Default::default(),
            backend: AgentBackendMetadata {
                name: "scripted".to_string(),
                version: None,
                session,
            },
        })
    }
}

async fn commit(engine: &ConfluenceEngine, invocation: &AgentInvocation, mutations: Vec<Mutation>) {
    let task = engine
        .resolve_token(&invocation.task_token)
        .expect("the task capability resolves");

    let base_revision = engine
        .task_context(task)
        .expect("task context")
        .snapshot_revision;

    let result = engine
        .submit(CommitRequest {
            task,
            patch_id: PatchId::fresh(),
            base_revision,
            patch: SpecPatch { mutations },
            client_nonce: Uuid::new_v4(),
        })
        .await
        .expect("the sequencer runs");

    if let Err(rejection) = result {
        panic!("scripted commit rejected: {rejection:?}");
    }
}

fn canonical(fields: &[(&str, ScalarType)]) -> Schema {
    Schema::Canonical(CanonicalSchema {
        description: None,
        completeness: SchemaCompleteness::Complete,
        fields: fields
            .iter()
            .map(|(name, ty)| {
                (
                    name.to_string(),
                    Field {
                        ty: TypeRef::Scalar(*ty),
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
            Input::Request(RequestInput {
                schema: id("schema.PingRequest"),
                identity: RequestIdentity::Keyed(conseqa::spec::RequestIdentityKey {
                    fields: vec![path("id")],
                }),
                result: ResultType {
                    ok: id("schema.PingResponse"),
                    err: ErrorResultType {
                        schema: id("schema.Rejected"),
                        disposition: ErrorDisposition::Terminal,
                    },
                },
            }),
        )]),
    }
}

fn ping_program() -> OperationBlock {
    OperationBlock {
        steps: vec![OperationStep::Return(Return {
            request: id("input.ping.request"),
            outcome: ResultOutcome::Ok {
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

const OBLIGATION: &str = "obl.serialize-ping";

/// The scripted plan that reaches success: decompose the skeleton,
/// synthesize the program, and propose the serialization requirement
/// mapped to the explicit prompt obligation.
fn success_script() -> ScriptFn {
    Arc::new(|engine, invocation| {
        Box::pin(async move {
            let task = engine
                .resolve_token(&invocation.task_token)
                .expect("token resolves");

            match invocation.kind {
                conseqa::confluence::TaskKind::Decompose => {
                    commit(
                        &engine,
                        &invocation,
                        vec![
                            Mutation::PutService {
                                id: id("service.api"),
                                value: Service {
                                    kind: ServiceKind::Backend,
                                },
                            },
                            Mutation::PutSchema {
                                id: id("schema.PingRequest"),
                                value: canonical(&[("id", ScalarType::Uuid)]),
                            },
                            Mutation::PutSchema {
                                id: id("schema.PingResponse"),
                                value: canonical(&[("id", ScalarType::Uuid)]),
                            },
                            Mutation::PutSchema {
                                id: id("schema.Rejected"),
                                value: canonical(&[("reason", ScalarType::String)]),
                            },
                            Mutation::PutOperationInterface {
                                operation: id("operation.ping"),
                                value: ping_interface(),
                            },
                            Mutation::PutPromptObligation {
                                id: PromptObligationId(OBLIGATION.to_string()),
                                value: PromptObligation {
                                    source_span: Some(
                                        "pings for the same id must never overlap".to_string(),
                                    ),
                                    normalized_intent: "serialize ping by id".to_string(),
                                    targets: vec![id("operation.ping")],
                                    status: PromptObligationStatus::Unmapped,
                                },
                            },
                        ],
                    )
                    .await;
                }

                // The runtime topology is authored in its own phase,
                // after L0 has converged and requirement discovery has
                // said what the runtime must discharge. The decomposer
                // cannot write it, and no operation-scoped task can.
                conseqa::confluence::TaskKind::TopologySynthesis => {
                    commit(
                        &engine,
                        &invocation,
                        vec![
                            Mutation::PutExecutionPool {
                                id: id("pool.ping_workers"),
                                value: conseqa::spec::ExecutionPool {
                                    member_concurrency: conseqa::spec::MemberConcurrency::Bounded(
                                        std::num::NonZeroU32::new(1).expect("non-zero"),
                                    ),
                                },
                            },
                            Mutation::PutRouter {
                                id: id("router.ping"),
                                value: conseqa::spec::Router {
                                    boundary: conseqa::spec::OperationInputRef {
                                        operation: id("operation.ping"),
                                        input: id("input.ping.request"),
                                    },
                                    pool: id("pool.ping_workers"),
                                    routing: Some(conseqa::spec::RequestRouting {
                                        key: vec![path("id")],
                                        member_assignment:
                                            conseqa::spec::MemberAssignment::ConsistentHash,
                                    }),
                                },
                            },
                        ],
                    )
                    .await;
                }

                conseqa::confluence::TaskKind::OperationSynthesis => {
                    // read-before-reference: the program references no
                    // external symbols (only its own input), so no
                    // extra reads are required beyond the bundle.
                    let _ = task;

                    commit(
                        &engine,
                        &invocation,
                        vec![
                            Mutation::ReplaceOperationProgram {
                                operation: id("operation.ping"),
                                program: ping_program(),
                            },
                        ],
                    )
                    .await;
                }

                conseqa::confluence::TaskKind::RequirementDiscovery => {
                    commit(
                        &engine,
                        &invocation,
                        vec![Mutation::ProposeRequirements {
                            operation: id("operation.ping"),
                            proposals: vec![RequirementSubmission {
                                requirement: ProposedRequirement::Serialization(
                                    SerializationRequirement {
                                        key: ValueRef {
                                            source: ValueSource::Input(id("input.ping.request")),
                                            path: path("id"),
                                        },
                                    },
                                ),
                                origin: RequirementOrigin::ExplicitPrompt {
                                    obligation: PromptObligationId(OBLIGATION.to_string()),
                                },
                            }],
                        }],
                    )
                    .await;
                }

                // No repair is needed: the requirement proves from the
                // router's semantic key and the pool's serial members.
                _ => {}
            }
        })
    })
}

/// The scripted plan for the incomplete path: same skeleton and
/// program, but discovery proposes an idempotency requirement the
/// architecture cannot prove (no deduplication), and repair changes
/// nothing.
fn incomplete_script() -> ScriptFn {
    Arc::new(|engine, invocation| {
        Box::pin(async move {
            match invocation.kind {
                conseqa::confluence::TaskKind::Decompose => {
                    commit(
                        &engine,
                        &invocation,
                        vec![
                            Mutation::PutService {
                                id: id("service.api"),
                                value: Service {
                                    kind: ServiceKind::Backend,
                                },
                            },
                            Mutation::PutSchema {
                                id: id("schema.PingRequest"),
                                value: canonical(&[("id", ScalarType::Uuid)]),
                            },
                            Mutation::PutSchema {
                                id: id("schema.PingResponse"),
                                value: canonical(&[("id", ScalarType::Uuid)]),
                            },
                            Mutation::PutSchema {
                                id: id("schema.Rejected"),
                                value: canonical(&[("reason", ScalarType::String)]),
                            },
                            Mutation::PutOperationInterface {
                                operation: id("operation.ping"),
                                value: ping_interface(),
                            },
                            Mutation::PutPromptObligation {
                                id: PromptObligationId(OBLIGATION.to_string()),
                                value: PromptObligation {
                                    source_span: Some(
                                        "duplicate pings must collapse".to_string(),
                                    ),
                                    normalized_intent: "ping is idempotent by id".to_string(),
                                    targets: vec![id("operation.ping")],
                                    status: PromptObligationStatus::Unmapped,
                                },
                            },
                        ],
                    )
                    .await;
                }

                // The runtime topology is authored in its own phase,
                // after L0 has converged and requirement discovery has
                // said what the runtime must discharge. The decomposer
                // cannot write it, and no operation-scoped task can.
                conseqa::confluence::TaskKind::TopologySynthesis => {
                    commit(
                        &engine,
                        &invocation,
                        vec![
                            Mutation::PutExecutionPool {
                                id: id("pool.ping_workers"),
                                value: conseqa::spec::ExecutionPool {
                                    member_concurrency: conseqa::spec::MemberConcurrency::Bounded(
                                        std::num::NonZeroU32::new(1).expect("non-zero"),
                                    ),
                                },
                            },
                            Mutation::PutRouter {
                                id: id("router.ping"),
                                value: conseqa::spec::Router {
                                    boundary: conseqa::spec::OperationInputRef {
                                        operation: id("operation.ping"),
                                        input: id("input.ping.request"),
                                    },
                                    pool: id("pool.ping_workers"),
                                    routing: Some(conseqa::spec::RequestRouting {
                                        key: vec![path("id")],
                                        member_assignment:
                                            conseqa::spec::MemberAssignment::ConsistentHash,
                                    }),
                                },
                            },
                        ],
                    )
                    .await;
                }

                conseqa::confluence::TaskKind::OperationSynthesis => {
                    commit(
                        &engine,
                        &invocation,
                        vec![
                            Mutation::ReplaceOperationProgram {
                                operation: id("operation.ping"),
                                program: ping_program(),
                            },
                        ],
                    )
                    .await;
                }

                conseqa::confluence::TaskKind::RequirementDiscovery => {
                    // A guaranteed-recoverability requirement: the echo
                    // program has no modeled retry driver, so it cannot
                    // be proven, and it is tied to the explicit
                    // obligation so it must not be dropped.
                    commit(
                        &engine,
                        &invocation,
                        vec![Mutation::ProposeRequirements {
                            operation: id("operation.ping"),
                            proposals: vec![RequirementSubmission {
                                requirement: ProposedRequirement::Recoverability(
                                    conseqa::spec::RecoverabilityRequirement {
                                        key: IdempotencyKey {
                                            components: vec![ValueRef {
                                                source: ValueSource::Input(id(
                                                    "input.ping.request",
                                                )),
                                                path: path("id"),
                                            }],
                                        },
                                        completion:
                                            conseqa::spec::CompletionRequirement::Guaranteed,
                                    },
                                ),
                                origin: RequirementOrigin::ExplicitPrompt {
                                    obligation: PromptObligationId(OBLIGATION.to_string()),
                                },
                            }],
                        }],
                    )
                    .await;
                }

                // Repair cannot fix it without real reasoning; commit
                // nothing so the obligation stays unproven.
                _ => {}
            }
        })
    })
}

/// A plan that builds a valid model with no requirements at all: the
/// prompt states no explicit obligation, and discovery proposes nothing.
/// The run should still converge — a requirement-free model is vacuously
/// proven — and it must finalize promptly rather than re-running
/// discovery every iteration until the budget is spent.
fn no_requirements_script() -> ScriptFn {
    Arc::new(|engine, invocation| {
        Box::pin(async move {
            match invocation.kind {
                conseqa::confluence::TaskKind::Decompose => {
                    commit(
                        &engine,
                        &invocation,
                        vec![
                            Mutation::PutService {
                                id: id("service.api"),
                                value: Service {
                                    kind: ServiceKind::Backend,
                                },
                            },
                            Mutation::PutSchema {
                                id: id("schema.PingRequest"),
                                value: canonical(&[("id", ScalarType::Uuid)]),
                            },
                            Mutation::PutSchema {
                                id: id("schema.PingResponse"),
                                value: canonical(&[("id", ScalarType::Uuid)]),
                            },
                            Mutation::PutSchema {
                                id: id("schema.Rejected"),
                                value: canonical(&[("reason", ScalarType::String)]),
                            },
                            Mutation::PutOperationInterface {
                                operation: id("operation.ping"),
                                value: ping_interface(),
                            },
                        ],
                    )
                    .await;
                }

                conseqa::confluence::TaskKind::OperationSynthesis => {
                    commit(
                        &engine,
                        &invocation,
                        vec![
                            Mutation::ReplaceOperationProgram {
                                operation: id("operation.ping"),
                                program: ping_program(),
                            },
                        ],
                    )
                    .await;
                }

                // Discovery proposes nothing; repair has nothing to do.
                _ => {}
            }
        })
    })
}

fn workflow(
    out_dir: PathBuf,
    script: ScriptFn,
    max_iterations: u32,
) -> (Workflow, ConfluenceEngine) {
    workflow_with_objective(out_dir, script, max_iterations, None)
}

/// The same harness, with a run objective layered on as `request_design`
/// supplies one.
fn workflow_with_objective(
    out_dir: PathBuf,
    script: ScriptFn,
    max_iterations: u32,
    objective: Option<String>,
) -> (Workflow, ConfluenceEngine) {
    let mut run_meta = RunMetadata::new(RunId("workflow-test".to_string()));
    run_meta.prompt = Some("A ping service.".to_string());
    run_meta.policy = RunPolicy {
        strict_requirements: true,
        adopt_recommended: false,
    };

    let engine =
        ConfluenceEngine::in_memory(WorkspaceState::empty(run_meta)).expect("engine starts");

    let backend = Arc::new(ScriptedBackend {
        engine: engine.clone(),
        script,
    });

    let supervisor = Supervisor::new(
        engine.clone(),
        backend,
        "http://127.0.0.1:0/mcp",
        None,
        out_dir.join("work"),
    );

    let scheduler = Scheduler::new(engine.clone(), supervisor, SchedulerPolicy::default());

    let workflow = Workflow::new(
        scheduler,
        WorkflowConfig {
            out_dir,
            analysis_timeout: Duration::from_secs(20),
            max_iterations,
            objective,
        },
    );

    (workflow, engine)
}

#[tokio::test]
async fn prompt_to_validated_model_with_all_requirements_proven() {
    let out_dir = std::env::temp_dir().join(format!("conseqa-wf-{}", Uuid::new_v4()));

    let (workflow, engine) = workflow(out_dir.clone(), success_script(), 8);

    let report = workflow.run().await.expect("the workflow runs");

    // Success: validated head, requirement proven, obligation mapped.
    assert!(
        matches!(report.status, RunStatus::Success { .. }),
        "{:?}",
        report.status
    );

    // The finalized artifacts were written (§76).
    let yaml = out_dir.join("conseqa.yaml");
    let verification = out_dir.join("verification-report.json");
    let manifest = out_dir.join("confluence-manifest.json");

    assert!(yaml.exists());
    assert!(verification.exists());
    assert!(manifest.exists());

    // The exported YAML round-trips through the standalone checker with
    // the requirement proven.
    let source = std::fs::read_to_string(&yaml).expect("yaml readable");
    let model = conseqa::parser::yaml::parse(&source).expect("exported model parses");

    let errors = conseqa::analyzer::validate(&model);
    assert!(errors.is_empty(), "{errors:?}");

    let checked = conseqa::analyzer::verification::verify(&model);
    assert!(checked.all_proven(), "the exported model verifies");

    // The prompt obligation ended mapped (§71).
    let head = engine.head_snapshot();
    let obligation = &head.workspace.prompt_obligations[&PromptObligationId(OBLIGATION.to_string())];

    assert!(matches!(
        obligation.status,
        PromptObligationStatus::Mapped { .. }
    ));

    // The manifest records the mapping and success.
    let manifest_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest).expect("manifest readable"))
            .expect("manifest is json");

    assert_eq!(manifest_json["status"]["kind"], "success");
    assert_eq!(manifest_json["backend"], "scripted");
    assert!(
        manifest_json["prompt_obligations"][OBLIGATION]
            .as_str()
            .unwrap_or_default()
            .starts_with("mapped"),
        "{manifest_json}"
    );

    std::fs::remove_dir_all(&out_dir).ok();
}

#[tokio::test]
async fn an_unprovable_obligation_yields_incomplete_preserving_the_gap() {
    let out_dir = std::env::temp_dir().join(format!("conseqa-wf-{}", Uuid::new_v4()));

    // A generous budget: the loop must recognize that repair committed
    // nothing and stop, rather than spending the budget re-running
    // identical tasks against an identical snapshot.
    let (workflow, engine) = workflow(out_dir.clone(), incomplete_script(), 16);

    let report = workflow.run().await.expect("the workflow runs");

    let RunStatus::Incomplete { unresolved, .. } = &report.status else {
        panic!("expected incomplete, got {:?}", report.status);
    };

    assert!(
        report.iterations < 16,
        "the stuck obstacle burned the whole iteration budget: {}",
        report.iterations
    );

    // The unresolved recoverability obligation is preserved (§75).
    assert!(
        unresolved
            .iter()
            .any(|entry| entry.contains("recoverability") && entry.contains("operation.ping")),
        "{unresolved:?}"
    );

    // The requirement was never silently dropped to make the run pass
    // (§70): it is still declared on the operation.
    let head = engine.head_snapshot();
    let draft = &head.workspace.operations[&id("operation.ping")];

    assert_eq!(draft.requirements.recoverability.len(), 1);

    std::fs::remove_dir_all(&out_dir).ok();
}

#[tokio::test]
async fn workers_that_build_nothing_yield_incomplete_not_false_success() {
    // The failure a live run hit: workers produced no architecture, and
    // the operation-less model was vacuously "all proven", so the run
    // reported success. It must report Incomplete instead.
    let out_dir = std::env::temp_dir().join(format!("conseqa-wf-{}", Uuid::new_v4()));

    // A backend that commits nothing, whatever the task.
    let script: ScriptFn = Arc::new(|_engine, _invocation| Box::pin(async {}));

    let (workflow, engine) = workflow(out_dir.clone(), script, 2);

    let report = workflow.run().await.expect("the workflow runs");

    let RunStatus::Incomplete { reason, .. } = &report.status else {
        panic!("expected incomplete, got {:?}", report.status);
    };

    assert!(
        reason.contains("no operations were synthesized"),
        "{reason}"
    );

    // Nothing was committed, so the head never advanced past empty.
    assert!(engine.head_snapshot().workspace.operations.is_empty());

    std::fs::remove_dir_all(&out_dir).ok();
}

/// A workspace with `count` planned operations, each a subscription on
/// one shared topic — the setup an operation-synthesis fanout starts
/// from.
fn planned_workspace(count: usize) -> WorkspaceState {
    use conseqa::spec::{
        MessageSelector,
        SubscriptionInput, Topic,
    };

    let mut workspace = WorkspaceState::empty(RunMetadata::new(RunId("fanout".to_string())));

    workspace.services.insert(
        id("service.api"),
        Service {
            kind: ServiceKind::Backend,
        },
    );

    workspace
        .schemas
        .insert(id("schema.Event"), canonical(&[("id", ScalarType::Uuid)]));

    workspace.topics.insert(
        id("topic.events"),
        Topic {
            messages: [id("schema.Event")].into_iter().collect(),
            message_identity: conseqa::spec::MessageIdentity::Unspecified,
        },
    );

    for index in 0..count {
        let operation = id(&format!("operation.worker{index}"));
        let input = id(&format!("input.worker{index}.event"));

        workspace.operations.insert(
            operation,
            conseqa::confluence::DraftOperation::planned(OperationInterfaceDraft {
                service: id("service.api"),
                description: Some(format!("Worker {index}.")),
                inputs: BTreeMap::from([(
                    input,
                    Input::Subscription(SubscriptionInput {
                        topic: id("topic.events"),
                        messages: MessageSelector::Only(
                            [id("schema.Event")].into_iter().collect(),
                        ),
                    }),
                )]),
            }),
        );
    }

    workspace
}

/// The operation a task's write scope names, read from its context —
/// how the scripted agent learns which operation it owns.
fn scoped_operation(engine: &ConfluenceEngine, invocation: &AgentInvocation) -> Option<Id> {
    use conseqa::confluence::WriteGrant;

    let task = engine.resolve_token(&invocation.task_token)?;
    let context = engine.task_context(task).ok()?;

    context.write_scope.grants.iter().find_map(|grant| match grant {
        WriteGrant::OperationProgram(operation) => Some(operation.clone()),
        _ => None,
    })
}

#[tokio::test]
async fn operation_fanout_runs_agents_concurrently() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    const OPERATIONS: usize = 4;

    let engine =
        ConfluenceEngine::in_memory(planned_workspace(OPERATIONS)).expect("engine starts");

    // Each session records the concurrent-session high-water mark, then
    // holds itself open long enough for the others to overlap before
    // committing its own operation's program.
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    let script: ScriptFn = {
        let active = active.clone();
        let peak = peak.clone();

        Arc::new(move |engine, invocation| {
            let active = active.clone();
            let peak = peak.clone();

            Box::pin(async move {
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);

                // Hold the session open so genuinely concurrent runs
                // overlap; a sequential scheduler would serialize these.
                tokio::time::sleep(Duration::from_millis(200)).await;

                active.fetch_sub(1, Ordering::SeqCst);

                let operation =
                    scoped_operation(&engine, &invocation).expect("a program scope");

                commit(
                    &engine,
                    &invocation,
                    vec![
                        Mutation::ReplaceOperationProgram {
                            operation,
                            program: OperationBlock {
                                steps: vec![OperationStep::Complete],
                            },
                        },
                    ],
                )
                .await;
            })
        })
    };

    let backend = Arc::new(ScriptedBackend {
        engine: engine.clone(),
        script,
    });

    let supervisor = Supervisor::new(
        engine.clone(),
        backend,
        "http://127.0.0.1:0/mcp",
        None,
        std::env::temp_dir().join(format!("conseqa-fanout-{}", Uuid::new_v4())),
    );

    let scheduler = Scheduler::new(
        engine.clone(),
        supervisor,
        SchedulerPolicy {
            max_attempts: 1,
            max_concurrent_agents: OPERATIONS,
            ..Default::default()
        },
    );

    let tasks: Vec<conseqa::harness::LogicalTask> = (0..OPERATIONS)
        .map(|index| conseqa::harness::LogicalTask {
            kind: conseqa::confluence::TaskKind::OperationSynthesis,
            objective: format!("synthesize worker{index}"),
            write_scope: conseqa::confluence::WriteScope::operation_synthesis(id(&format!(
                "operation.worker{index}"
            ))),
            bundle: conseqa::confluence::BundleSpec {
                operation: Some(id(&format!("operation.worker{index}"))),
                requirement: None,
                include: Vec::new(),
            },
            prompt_evidence: Vec::new(),
            interactive: false,
        })
        .collect();

    let started = std::time::Instant::now();

    let runs = scheduler.run_many(tasks).await.expect("the fanout runs");

    let elapsed = started.elapsed();

    // Every session committed its operation's program.
    assert_eq!(runs.len(), OPERATIONS);
    assert!(runs.iter().all(|run| run.committed()), "{runs:?}");

    // The sessions genuinely overlapped: all four were active at once.
    // A sequential scheduler would peak at 1.
    assert_eq!(
        peak.load(Ordering::SeqCst),
        OPERATIONS,
        "expected all {OPERATIONS} sessions active simultaneously"
    );

    // And the wall time reflects concurrency: four 200ms sessions in
    // parallel finish far under the ~800ms a sequential run would take.
    assert!(
        elapsed < Duration::from_millis(600),
        "fanout took {elapsed:?}, which looks sequential"
    );

    // All four commits landed through the one sequencer, at distinct
    // revisions.
    assert_eq!(engine.head_revision().0, OPERATIONS as u64);
}

#[tokio::test]
async fn a_requirement_free_model_finalizes_without_spinning() {
    let out_dir = std::env::temp_dir().join(format!("conseqa-wf-{}", Uuid::new_v4()));

    // A generous iteration budget: if the fixpoint re-ran discovery every
    // iteration (gating on tasks run rather than progress), it would spend
    // the whole budget before finalizing.
    let (workflow, _engine) = workflow(out_dir.clone(), no_requirements_script(), 8);

    let report = workflow.run().await.expect("the workflow runs");

    // A requirement-free model is vacuously proven, so the run succeeds.
    assert!(
        matches!(report.status, RunStatus::Success { .. }),
        "{:?}",
        report.status
    );

    // And it finalized promptly: discovery committed nothing, so the loop
    // proceeded to verify and finalize instead of spinning to the bound.
    assert!(
        report.iterations <= 2,
        "expected a prompt finalize, but the run took {} iterations",
        report.iterations
    );

    std::fs::remove_dir_all(&out_dir).ok();
}

/// `request_design`'s objective is layered onto the project prompt as
/// task evidence, so every fanned-out worker sees the caller's steer.
/// It used to be accepted and discarded.
#[tokio::test]
async fn a_run_objective_reaches_every_worker_prompt() {
    let out_dir = std::env::temp_dir().join(format!("conseqa-wf-{}", Uuid::new_v4()));

    let prompts: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = Arc::clone(&prompts);

    // Capture each worker's rendered prompt; commit nothing, so the run
    // simply reaches its iteration bound.
    let script: ScriptFn = Arc::new(move |_engine, invocation| {
        let captured = Arc::clone(&captured);

        Box::pin(async move {
            captured
                .lock()
                .expect("prompt lock")
                .push(invocation.prompt.clone());
        })
    });

    let (workflow, _engine) = workflow_with_objective(
        out_dir.clone(),
        script,
        1,
        Some("prioritize the checkout path".to_string()),
    );

    workflow.run().await.expect("the workflow runs");

    let prompts = prompts.lock().expect("prompt lock");

    assert!(!prompts.is_empty(), "at least one worker ran");
    assert!(
        prompts
            .iter()
            .all(|prompt| prompt.contains("prioritize the checkout path")),
        "every worker prompt carries the run objective: {prompts:#?}"
    );
    assert!(
        prompts.iter().all(|prompt| prompt.contains("A ping service.")),
        "the project prompt is still carried too: {prompts:#?}"
    );

    std::fs::remove_dir_all(&out_dir).ok();
}
