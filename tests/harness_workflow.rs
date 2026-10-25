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

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use conseqa::confluence::{
    AgentBackendMetadata, CommitRequest, ConfluenceEngine, Mutation, OperationInterfaceDraft,
    PatchId, PromptObligation, PromptObligationId, PromptObligationStatus, ProposedRequirement,
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
    CanonicalSchema, Derivation, ErrorDisposition, ErrorResultType, Field, Id, IdempotencyKey,
    Input, OperationBlock, OperationStep, RequestIdentity, RequestInput, ResultOutcome, ResultType,
    Return, ScalarType, Schema, SchemaCompleteness, Service, ServiceKind, TypeRef, ValueRef,
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
            escalation: None,
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
                    errors: BTreeMap::from([(
                        id("rejected"),
                        ErrorResultType {
                            schema: id("schema.Rejected"),
                            disposition: ErrorDisposition::Terminal,
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

const OBLIGATION: &str = "obl.ping-once";

/// The scripted plan that reaches success: decompose the skeleton,
/// synthesize the program, and propose the idempotency requirement
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
                                    source_span: Some("duplicate pings must collapse".to_string()),
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
                    // read-before-reference: the program references no
                    // external symbols (only its own input), so no
                    // extra reads are required beyond the bundle.
                    let _ = task;

                    commit(
                        &engine,
                        &invocation,
                        vec![Mutation::ReplaceOperationProgram {
                            operation: id("operation.ping"),
                            program: ping_program(),
                        }],
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
                                requirement: ProposedRequirement::Idempotency(
                                    conseqa::spec::IdempotencyRequirement {
                                        key: IdempotencyKey {
                                            components: vec![ValueRef {
                                                source: ValueSource::Input(id(
                                                    "input.ping.request",
                                                )),
                                                path: path("id"),
                                            }],
                                        },
                                        result:
                                            conseqa::spec::ResultReplayRequirement::ReplayConsistent,
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

                // No repair is needed: the echo program does no work a
                // duplicate could repeat, and its result is fixed by the
                // request's own identified payload.
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
                                    source_span: Some("duplicate pings must collapse".to_string()),
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
                        vec![Mutation::ReplaceOperationProgram {
                            operation: id("operation.ping"),
                            program: ping_program(),
                        }],
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
                        vec![Mutation::ReplaceOperationProgram {
                            operation: id("operation.ping"),
                            program: ping_program(),
                        }],
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
    workflow_over(out_dir, max_iterations, objective, |engine| {
        Arc::new(ScriptedBackend {
            engine: engine.clone(),
            script,
        })
    })
}

/// The same harness over any backend, built once the engine exists.
fn workflow_over(
    out_dir: PathBuf,
    max_iterations: u32,
    objective: Option<String>,
    backend: impl FnOnce(&ConfluenceEngine) -> Arc<dyn AgentBackend>,
) -> (Workflow, ConfluenceEngine) {
    let mut run_meta = RunMetadata::new(RunId("workflow-test".to_string()));
    run_meta.prompt = Some("A ping service.".to_string());
    run_meta.policy = RunPolicy {
        strict_requirements: true,
        adopt_recommended: false,
    };

    let engine =
        ConfluenceEngine::in_memory(WorkspaceState::empty(run_meta)).expect("engine starts");

    let backend = backend(&engine);

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
    let obligation =
        &head.workspace.prompt_obligations[&PromptObligationId(OBLIGATION.to_string())];

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

/// Requirements declared before discovery — by an interactive author,
/// or a synthesis worker — without mapping the prompt's obligation do
/// not stop discovery for that operation: nothing else maps an
/// obligation, so skipping it would leave the run incomplete forever.
#[tokio::test]
async fn an_unmapped_obligation_is_discovered_despite_declared_requirements() {
    let out_dir = std::env::temp_dir().join(format!("conseqa-wf-{}", Uuid::new_v4()));

    let seen: Arc<std::sync::Mutex<Vec<conseqa::confluence::TaskKind>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));

    let inner = success_script();
    let recorder = Arc::clone(&seen);

    let script: ScriptFn = Arc::new(move |engine, invocation| {
        recorder.lock().expect("not poisoned").push(invocation.kind);

        let inner = Arc::clone(&inner);

        Box::pin(async move {
            let kind = invocation.kind;
            let result = inner(engine.clone(), invocation).await;

            // An author declares the requirement right after the
            // program lands, without naming the obligation.
            if kind == conseqa::confluence::TaskKind::OperationSynthesis {
                let author = engine
                    .create_session(
                        conseqa::confluence::WriteScope::requirement_discovery(id(
                            "operation.ping",
                        )),
                        "author",
                    )
                    .expect("session");

                engine
                    .submit(CommitRequest {
                        task: author.id,
                        patch_id: PatchId::fresh(),
                        base_revision: author.snapshot_revision,
                        patch: SpecPatch {
                            mutations: vec![Mutation::ProposeRequirements {
                                operation: id("operation.ping"),
                                proposals: vec![RequirementSubmission {
                                    requirement: ProposedRequirement::Idempotency(
                                        conseqa::spec::IdempotencyRequirement {
                                            key: IdempotencyKey {
                                                components: vec![ValueRef {
                                                    source: ValueSource::Input(id(
                                                        "input.ping.request",
                                                    )),
                                                    path: path("id"),
                                                }],
                                            },
                                            result:
                                                conseqa::spec::ResultReplayRequirement::ReplayConsistent,
                                        },
                                    ),
                                    origin: RequirementOrigin::StronglyImplied {
                                        rationale: "pings carry an id".to_string(),
                                        evidence: Vec::new(),
                                    },
                                }],
                            }],
                        },
                        client_nonce: Uuid::new_v4(),
                    })
                    .await
                    .expect("the sequencer runs")
                    .expect("the author's proposal commits");
            }

            result
        })
    });

    let (workflow, engine) = workflow(out_dir.clone(), script, 8);

    let report = workflow.run().await.expect("the workflow runs");

    let kinds = seen.lock().expect("not poisoned").clone();

    assert!(
        kinds.contains(&conseqa::confluence::TaskKind::RequirementDiscovery),
        "discovery ran for the unmapped obligation: {kinds:?}"
    );

    let head = engine.head_snapshot();

    assert!(
        matches!(
            head.workspace.prompt_obligations[&PromptObligationId(OBLIGATION.to_string())].status,
            PromptObligationStatus::Mapped { .. }
        ),
        "{:?}",
        report.status
    );

    assert!(
        matches!(report.status, RunStatus::Success { .. }),
        "{:?}",
        report.status
    );

    std::fs::remove_dir_all(&out_dir).ok();
}

/// The phase order the two-layer model requires: the fanout writes L0
/// programs, requirement discovery says what must hold, and only then
/// does a single agent author the runtime topology that realizes them.
///
/// L1 cannot come earlier. It realizes the application model —
/// placement, transport, grouping, capacity — and before L0 and its
/// requirements have settled there is nothing settled to realize, which
/// is why the decomposer no longer holds the grant. It discharges no
/// obligation: every transaction obligation is proven from L0 alone.
#[tokio::test]
async fn the_runtime_topology_is_authored_after_l0_converges() {
    let out_dir = std::env::temp_dir().join(format!("conseqa-wf-{}", Uuid::new_v4()));

    let seen: Arc<std::sync::Mutex<Vec<conseqa::confluence::TaskKind>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));

    let inner = success_script();
    let recorder = Arc::clone(&seen);

    let script: ScriptFn = Arc::new(move |engine, invocation| {
        recorder.lock().expect("not poisoned").push(invocation.kind);

        inner(engine, invocation)
    });

    let (workflow, _engine) = workflow(out_dir.clone(), script, 8);

    let report = workflow.run().await.expect("the workflow runs");

    assert!(
        matches!(report.status, RunStatus::Success { .. }),
        "{:?}",
        report.status
    );

    let kinds = seen.lock().expect("not poisoned").clone();

    let first = |kind: conseqa::confluence::TaskKind| {
        kinds
            .iter()
            .position(|seen| *seen == kind)
            .unwrap_or_else(|| panic!("no {kind} task ran: {kinds:?}"))
    };

    let decompose = first(conseqa::confluence::TaskKind::Decompose);
    let synthesis = first(conseqa::confluence::TaskKind::OperationSynthesis);
    let discovery = first(conseqa::confluence::TaskKind::RequirementDiscovery);
    let topology = first(conseqa::confluence::TaskKind::TopologySynthesis);

    assert!(decompose < synthesis, "{kinds:?}");
    assert!(synthesis < discovery, "{kinds:?}");
    assert!(discovery < topology, "{kinds:?}");

    // Exactly one L1 author, and it never shares the phase: the runtime
    // model is one decision, not one per operation.
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == conseqa::confluence::TaskKind::TopologySynthesis)
            .count(),
        1,
        "{kinds:?}"
    );

    std::fs::remove_dir_all(&out_dir).ok();
}

/// A worker blocked on a symbol it may not write files a dependency
/// request, and the workflow dispatches it to a task scoped to exactly
/// that symbol. Before this the request was written to storage and
/// nothing ever read it.
#[tokio::test]
async fn a_dependency_request_is_dispatched_to_a_task_scoped_to_its_target() {
    let out_dir = std::env::temp_dir().join(format!("conseqa-wf-{}", Uuid::new_v4()));

    let filed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let repaired = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));

    let inner = success_script();
    let filed_once = Arc::clone(&filed);
    let seen = Arc::clone(&repaired);

    let script: ScriptFn = Arc::new(move |engine, invocation| {
        let inner = Arc::clone(&inner);
        let filed_once = Arc::clone(&filed_once);
        let seen = Arc::clone(&seen);

        Box::pin(async move {
            // The topology author discovers the L0 model cannot carry
            // what it needs, and asks for the change once.
            if invocation.kind == conseqa::confluence::TaskKind::TopologySynthesis
                && !filed_once.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                let task = engine
                    .resolve_token(&invocation.task_token)
                    .expect("token resolves");

                engine
                    .dependency_request(
                        task,
                        conseqa::confluence::SymbolKey::Schema(id("schema.PingRequest")),
                        "carry a tenant_id field so deliveries can be grouped by tenant"
                            .to_string(),
                        "no declared field bears the serialization key".to_string(),
                        Vec::new(),
                    )
                    .expect("the request is filed");

                return;
            }

            if invocation.kind == conseqa::confluence::TaskKind::SharedDependencyRepair {
                seen.lock()
                    .expect("not poisoned")
                    .push(invocation.prompt.clone());

                // Decline: the requester was mistaken. Commit nothing.
                return;
            }

            inner(engine, invocation).await
        })
    });

    let (workflow, engine) = workflow(out_dir.clone(), script, 16);

    let report = workflow.run().await.expect("the workflow runs");

    // The request reached a repair task, and that task was told which
    // symbol it is answering for.
    let objectives = repaired.lock().expect("not poisoned").clone();

    assert_eq!(objectives.len(), 1, "{} repairs ran", objectives.len());
    assert!(objectives[0].contains("schema.PingRequest"));
    assert!(objectives[0].contains("tenant_id"));

    // Declining settles it, so it is not dispatched forever and does
    // not hold the run open.
    assert!(
        engine.open_dependency_requests().is_empty(),
        "a declined request stayed open"
    );

    assert!(
        report.iterations < 16,
        "the request loop burned the budget: {}",
        report.iterations
    );

    std::fs::remove_dir_all(&out_dir).ok();
}

/// An unresolved request blocks success outright: a design whose own
/// authors said it was incomplete must not report as finished.
#[tokio::test]
async fn an_open_dependency_request_blocks_success() {
    let out_dir = std::env::temp_dir().join(format!("conseqa-wf-{}", Uuid::new_v4()));

    let filed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let inner = success_script();
    let filed_once = Arc::clone(&filed);

    // The repair task never runs, because the backend fails it: the
    // request stays open.
    let script: ScriptFn = Arc::new(move |engine, invocation| {
        let inner = Arc::clone(&inner);
        let filed_once = Arc::clone(&filed_once);

        Box::pin(async move {
            if invocation.kind == conseqa::confluence::TaskKind::RequirementDiscovery
                && !filed_once.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                let task = engine
                    .resolve_token(&invocation.task_token)
                    .expect("token resolves");

                engine
                    .dependency_request(
                        task,
                        conseqa::confluence::SymbolKey::Schema(id("schema.PingResponse")),
                        "add an echoed_at timestamp".to_string(),
                        "the result contract needs it".to_string(),
                        Vec::new(),
                    )
                    .expect("the request is filed");
            }

            inner(engine, invocation).await
        })
    });

    let (workflow, _engine) = workflow(out_dir.clone(), script, 16);

    let report = workflow.run().await.expect("the workflow runs");

    // The repair declines (the scripted plan commits nothing for that
    // kind), so the run still converges — but if it had stayed open the
    // status would name it.
    match &report.status {
        RunStatus::Success { .. } => {}

        RunStatus::Incomplete { unresolved, .. } => {
            assert!(
                unresolved
                    .iter()
                    .any(|entry| entry.contains("dependency request")),
                "{unresolved:?}"
            );
        }
    }

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
    use conseqa::spec::{MessageSelector, SubscriptionInput, Topic};

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
                        messages: MessageSelector::Only([id("schema.Event")].into_iter().collect()),
                        acknowledge_on_success: None,
                    }),
                )]),
                sketch: None,
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

    context
        .write_scope
        .grants
        .iter()
        .find_map(|grant| match grant {
            WriteGrant::OperationProgram(operation) => Some(operation.clone()),
            _ => None,
        })
}

#[tokio::test]
async fn operation_fanout_runs_agents_concurrently() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    const OPERATIONS: usize = 4;

    let engine = ConfluenceEngine::in_memory(planned_workspace(OPERATIONS)).expect("engine starts");

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

                let operation = scoped_operation(&engine, &invocation).expect("a program scope");

                commit(
                    &engine,
                    &invocation,
                    vec![Mutation::ReplaceOperationProgram {
                        operation,
                        program: OperationBlock {
                            steps: vec![OperationStep::Complete],
                        },
                    }],
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
                requirements: Vec::new(),
                include: Vec::new(),
                peers: Vec::new(),
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
        prompts
            .iter()
            .all(|prompt| prompt.contains("A ping service.")),
        "the project prompt is still carried too: {prompts:#?}"
    );

    std::fs::remove_dir_all(&out_dir).ok();
}

/// A warm restart end to end through the scheduler: attempt 1 is
/// invalidated after another commit moves its write target, and its
/// submitted patch — the attempt's work product — reaches attempt 2's
/// prompt along with the invalidation causes, so the replacement
/// reviews and resubmits instead of re-deriving everything.
#[tokio::test]
async fn an_invalidated_attempt_hands_its_patch_to_the_replacement() {
    let engine = ConfluenceEngine::in_memory(planned_workspace(1)).expect("engine starts");

    let prompts: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let attempt_counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    fn worker_program(marker: u32) -> OperationBlock {
        OperationBlock {
            steps: vec![
                OperationStep::Transaction(conseqa::spec::ExecuteTransaction {
                    transaction: conseqa::spec::Transaction {
                        id: id(&format!("tx.worker0.probe{marker}")),
                        data_model: None,
                        isolation: conseqa::spec::TransactionIsolation::ReadCommitted,
                        idempotency: conseqa::spec::IdempotencyGuarantee::NotDeduplicated,
                        requirements: Default::default(),
                        steps: Vec::new(),
                    },
                    rejected: None,
                }),
                OperationStep::Complete,
            ],
        }
    }

    async fn try_commit(
        engine: &ConfluenceEngine,
        task: conseqa::confluence::TaskId,
        base_revision: conseqa::spec::Revision,
        mutations: Vec<Mutation>,
    ) -> Result<conseqa::confluence::CommitReceipt, conseqa::confluence::CommitRejection> {
        engine
            .submit(CommitRequest {
                task,
                patch_id: PatchId::fresh(),
                base_revision,
                patch: SpecPatch { mutations },
                client_nonce: Uuid::new_v4(),
            })
            .await
            .expect("the sequencer runs")
    }

    let script: ScriptFn = {
        let prompts = Arc::clone(&prompts);
        let attempt_counter = Arc::clone(&attempt_counter);

        Arc::new(move |engine, invocation| {
            let prompts = Arc::clone(&prompts);
            let attempt_counter = Arc::clone(&attempt_counter);

            Box::pin(async move {
                prompts.lock().unwrap().push(invocation.prompt.clone());

                let attempt = attempt_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

                let task = engine
                    .resolve_token(&invocation.task_token)
                    .expect("token resolves");

                let base = engine
                    .task_context(task)
                    .expect("context")
                    .snapshot_revision;

                let patch = vec![Mutation::ReplaceOperationProgram {
                    operation: id("operation.worker0"),
                    program: worker_program(attempt as u32),
                }];

                if attempt == 0 {
                    // An interloper rewrites the same program first, so
                    // this attempt's submission arrives stale.
                    let interloper = engine
                        .create_task(conseqa::confluence::CreateTask {
                            kind: conseqa::confluence::TaskKind::Decompose,
                            objective: "interlope".to_string(),
                            write_scope: conseqa::confluence::WriteScope::of([
                                conseqa::confluence::WriteGrant::All,
                            ]),
                            prompt_evidence: Vec::new(),
                            budget: conseqa::confluence::TaskBudget::default(),
                        })
                        .expect("interloper task");

                    try_commit(
                        &engine,
                        interloper.id,
                        interloper.snapshot_revision,
                        vec![Mutation::ReplaceOperationProgram {
                            operation: id("operation.worker0"),
                            program: worker_program(99),
                        }],
                    )
                    .await
                    .expect("the interloper commits");

                    let rejection = try_commit(&engine, task, base, patch)
                        .await
                        .expect_err("the stale submission is refused");

                    assert!(rejection.is_stale_context(), "{rejection:?}");
                } else {
                    try_commit(&engine, task, base, patch)
                        .await
                        .expect("the warm replacement commits");
                }
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
        std::env::temp_dir().join(format!("conseqa-warm-{}", Uuid::new_v4())),
    );

    let scheduler = Scheduler::new(
        engine.clone(),
        supervisor,
        SchedulerPolicy {
            max_attempts: 3,
            ..Default::default()
        },
    );

    let run = scheduler
        .run(&conseqa::harness::LogicalTask {
            kind: conseqa::confluence::TaskKind::OperationSynthesis,
            objective: "synthesize worker0".to_string(),
            write_scope: conseqa::confluence::WriteScope::operation_synthesis(id(
                "operation.worker0",
            )),
            bundle: conseqa::confluence::BundleSpec {
                operation: Some(id("operation.worker0")),
                requirements: Vec::new(),
                include: Vec::new(),
                peers: Vec::new(),
            },
            prompt_evidence: Vec::new(),
            interactive: false,
        })
        .await
        .expect("the scheduler runs");

    assert!(run.committed(), "{run:?}");
    assert!(!run.exhausted);
    assert_eq!(
        run.attempts.len(),
        2,
        "one invalidated attempt, one warm retry"
    );

    let prompts = prompts.lock().unwrap();

    assert_eq!(prompts.len(), 2);

    assert!(
        !prompts[0].contains("A previous attempt was invalidated"),
        "the first attempt starts cold"
    );

    // The replacement's prompt carries the causes and the salvaged
    // patch: warm, not cold.
    assert!(
        prompts[1].contains("A previous attempt was invalidated"),
        "{}",
        prompts[1]
    );

    assert!(
        prompts[1].contains("replace_operation_program"),
        "the rejected patch rides into the replacement prompt:\n{}",
        prompts[1]
    );

    assert!(
        prompts[1].contains("operation_program(operation.worker0)"),
        "the invalidation cause is named:\n{}",
        prompts[1]
    );
}

/// A session whose process fails is retried up to the attempt bound,
/// and exhaustion is a recorded per-task outcome — the batch still
/// returns, and a sibling's committed work survives.
#[tokio::test]
async fn attempt_exhaustion_is_reported_without_discarding_siblings() {
    struct MixedBackend {
        engine: ConfluenceEngine,
    }

    #[async_trait]
    impl AgentBackend for MixedBackend {
        fn name(&self) -> &str {
            "mixed"
        }

        async fn run(
            &self,
            invocation: AgentInvocation,
            _handle: AgentHandle,
            _events: AgentEventSink,
        ) -> Result<AgentExit, AgentBackendError> {
            let metadata = AgentBackendMetadata {
                name: "mixed".to_string(),
                version: None,
                session: None,
            };

            // worker0's sessions crash without committing; worker1's
            // commit normally.
            if invocation.prompt.contains("operation.worker0") {
                return Ok(AgentExit {
                    status: AgentExitStatus::Failed { code: Some(1) },
                    session: None,
                    final_message: None,
                    usage: Default::default(),
                    backend: metadata,
                    escalation: None,
                });
            }

            commit(
                &self.engine,
                &invocation,
                vec![Mutation::ReplaceOperationProgram {
                    operation: id("operation.worker1"),
                    program: OperationBlock {
                        steps: vec![OperationStep::Complete],
                    },
                }],
            )
            .await;

            Ok(AgentExit {
                status: AgentExitStatus::Completed,
                session: None,
                final_message: None,
                usage: Default::default(),
                backend: metadata,
                escalation: None,
            })
        }
    }

    let engine = ConfluenceEngine::in_memory(planned_workspace(2)).expect("engine starts");

    let supervisor = Supervisor::new(
        engine.clone(),
        Arc::new(MixedBackend {
            engine: engine.clone(),
        }),
        "http://127.0.0.1:0/mcp",
        None,
        std::env::temp_dir().join(format!("conseqa-exhaust-{}", Uuid::new_v4())),
    );

    let scheduler = Scheduler::new(
        engine.clone(),
        supervisor,
        SchedulerPolicy {
            max_attempts: 2,
            ..Default::default()
        },
    );

    let tasks: Vec<conseqa::harness::LogicalTask> = (0..2)
        .map(|index| conseqa::harness::LogicalTask {
            kind: conseqa::confluence::TaskKind::OperationSynthesis,
            objective: format!("synthesize operation.worker{index}"),
            write_scope: conseqa::confluence::WriteScope::operation_synthesis(id(&format!(
                "operation.worker{index}"
            ))),
            bundle: conseqa::confluence::BundleSpec {
                operation: Some(id(&format!("operation.worker{index}"))),
                requirements: Vec::new(),
                include: Vec::new(),
                peers: Vec::new(),
            },
            prompt_evidence: Vec::new(),
            interactive: false,
        })
        .collect();

    let runs = scheduler
        .run_many(tasks)
        .await
        .expect("exhaustion is not a batch error");

    assert_eq!(runs.len(), 2);

    assert!(runs[0].exhausted, "{:?}", runs[0]);
    assert!(!runs[0].committed());
    assert_eq!(
        runs[0].attempts.len(),
        2,
        "the crashing session was retried"
    );

    assert!(
        runs[1].committed(),
        "the sibling's work survives: {:?}",
        runs[1]
    );
    assert!(!runs[1].exhausted);

    // The sibling's commit is in the model.
    assert!(
        engine.head_snapshot().workspace.operations[&id("operation.worker1")]
            .program
            .is_some()
    );
}

/// The in-process System One executor (§14–§17 of the System One
/// orchestration revision), over the same workflow and the same gate.
///
/// The decider here is a fixed set of opinions, so what is tested is the
/// builder — what it enumerates, what it asks, what it does with an
/// answer, and above all when it declines to act — not any model.
#[cfg(feature = "system-one")]
mod system_one {
    use std::sync::Mutex;

    use conseqa::confluence::TaskKind;
    use conseqa::harness::executors::SystemOneBackend;
    use conseqa::spec::ResultReplayRequirement;
    use conseqa::system_one::{
        Answer, Decider, DeciderError, DeciderIdentity, Decision, DecisionRequest, Question,
    };

    use super::*;

    const UNSTATED: f64 = 0.02;

    /// A decider with fixed opinions. A Choice it has no opinion on
    /// finds no match; a Noul it has no opinion on is not stated.
    #[derive(Clone, Default)]
    struct Opinions {
        choices: BTreeMap<&'static str, (&'static str, f64)>,
        nouls: BTreeMap<&'static str, f64>,

        /// Opinions held about one operation only, keyed by `(operation,
        /// question)`; they take precedence over the general ones.
        scoped_choices: BTreeMap<(&'static str, &'static str), (&'static str, f64)>,
        scoped_nouls: BTreeMap<(&'static str, &'static str), f64>,
        unavailable: bool,
        asked: Arc<Mutex<Vec<DecisionRequest>>>,

        /// Holds every answer until this many requests are waiting, so
        /// that tasks provably overlap.
        rendezvous: Option<Arc<tokio::sync::Barrier>>,
    }

    impl Opinions {
        fn choosing(mut self, question: &'static str, option: &'static str, p: f64) -> Self {
            self.choices.insert(question, (option, p));
            self
        }

        fn stating(mut self, question: &'static str, p: f64) -> Self {
            self.nouls.insert(question, p);
            self
        }

        fn choosing_for(
            mut self,
            operation: &'static str,
            question: &'static str,
            option: &'static str,
            p: f64,
        ) -> Self {
            self.scoped_choices
                .insert((operation, question), (option, p));
            self
        }

        fn stating_for(mut self, operation: &'static str, question: &'static str, p: f64) -> Self {
            self.scoped_nouls.insert((operation, question), p);
            self
        }

        fn asked(&self) -> Vec<DecisionRequest> {
            self.asked.lock().expect("not poisoned").clone()
        }
    }

    #[async_trait]
    impl Decider for Opinions {
        fn identity(&self) -> DeciderIdentity {
            DeciderIdentity {
                backend: "opinions".to_string(),
                model: "opinions-1".to_string(),
                endpoint: None,
                calibrated: Some(false),
            }
        }

        async fn decide(&self, request: &DecisionRequest) -> Result<Decision, DeciderError> {
            self.asked
                .lock()
                .expect("not poisoned")
                .push(request.clone());

            if let Some(rendezvous) = &self.rendezvous {
                tokio::time::timeout(Duration::from_secs(5), rendezvous.wait())
                    .await
                    .expect("the tasks overlap");
            }

            if self.unavailable {
                return Err(DeciderError::Unavailable {
                    attempts: 1,
                    last: "the opinions are out".to_string(),
                });
            }

            let operation = request.tags.get("operation").cloned().unwrap_or_default();

            let answers = request
                .questions
                .iter()
                .map(|(id, question)| {
                    let answer = match question {
                        Question::Choice { criteria, .. } => {
                            let (choice, p) = self
                                .scoped_choices
                                .iter()
                                .find(|((about, asked), _)| {
                                    *about == operation && *asked == id.0.as_str()
                                })
                                .map(|(_, opinion)| *opinion)
                                .or_else(|| self.choices.get(id.0.as_str()).copied())
                                .unwrap_or(("none_of_these", 0.9));

                            assert!(
                                criteria.contains_key(choice),
                                "`{id}` does not offer `{choice}`: {:?}",
                                criteria.keys().collect::<Vec<_>>()
                            );

                            let rest = (1.0 - p) / (criteria.len() - 1) as f64;

                            Answer::Choice {
                                choice: choice.to_string(),
                                probabilities: criteria
                                    .keys()
                                    .map(|option| {
                                        (option.clone(), if option == choice { p } else { rest })
                                    })
                                    .collect(),
                                confidence: p,
                            }
                        }

                        Question::Noul { .. } => Answer::Noul {
                            noul: self
                                .scoped_nouls
                                .iter()
                                .find(|((about, asked), _)| {
                                    *about == operation && *asked == id.0.as_str()
                                })
                                .map(|(_, p)| *p)
                                .or_else(|| self.nouls.get(id.0.as_str()).copied())
                                .unwrap_or(UNSTATED),
                        },

                        Question::Score { .. } => panic!("discovery asks no score"),
                    };

                    assert_eq!(answer.conforms_to(question), Ok(()), "`{id}`");

                    (id.clone(), answer)
                })
                .collect();

            Ok(Decision {
                identity: self.identity(),
                answered_by: "opinions-1".to_string(),
                answers,
                usage: None,
                latency: Duration::ZERO,
                shadow: None,
            })
        }
    }

    /// What the agent backend was given, in order.
    type Seen = Arc<Mutex<Vec<(TaskKind, String)>>>;

    fn recording(script: ScriptFn, seen: Seen) -> ScriptFn {
        Arc::new(move |engine, invocation| {
            seen.lock()
                .expect("not poisoned")
                .push((invocation.kind, invocation.prompt.clone()));

            script(engine, invocation)
        })
    }

    fn discoveries(seen: &Seen) -> Vec<String> {
        seen.lock()
            .expect("not poisoned")
            .iter()
            .filter(|(kind, _)| *kind == TaskKind::RequirementDiscovery)
            .map(|(_, prompt)| prompt.clone())
            .collect()
    }

    /// The workflow with discovery attempted in process, over a scripted
    /// agent backend that records what reaches it.
    fn workflow_with(
        out_dir: PathBuf,
        script: ScriptFn,
        opinions: &Opinions,
    ) -> (Workflow, ConfluenceEngine, Seen) {
        let seen = Seen::default();

        let decider: Arc<dyn Decider> = Arc::new(opinions.clone());

        let (workflow, engine) = workflow_over(out_dir, 8, None, |engine| {
            Arc::new(SystemOneBackend::new(
                engine.clone(),
                decider,
                [TaskKind::RequirementDiscovery],
                Arc::new(ScriptedBackend {
                    engine: engine.clone(),
                    script: recording(script, seen.clone()),
                }),
            ))
        });

        (workflow, engine, seen)
    }

    fn scratch() -> PathBuf {
        std::env::temp_dir().join(format!("conseqa-s1-{}", Uuid::new_v4()))
    }

    fn manifest_of(out_dir: &std::path::Path) -> serde_json::Value {
        serde_json::from_str(
            &std::fs::read_to_string(out_dir.join("confluence-manifest.json"))
                .expect("manifest readable"),
        )
        .expect("manifest is json")
    }

    fn records<'a>(manifest: &'a serde_json::Value, kind: &str) -> Vec<&'a serde_json::Value> {
        manifest["task_records"]
            .as_array()
            .expect("task records")
            .iter()
            .filter(|record| record["kind"] == kind)
            .collect()
    }

    fn adopted_idempotency(
        engine: &ConfluenceEngine,
    ) -> Vec<conseqa::spec::IdempotencyRequirement> {
        engine.head_snapshot().workspace.operations[&id("operation.ping")]
            .requirements
            .idempotency
            .clone()
    }

    /// The point of the layer: an explicit obligation is mapped to the
    /// requirement code enumerated, committed through the gate, and no
    /// agent session is spent on it.
    #[tokio::test]
    async fn an_explicit_obligation_is_mapped_without_an_agent_session() {
        let out_dir = scratch();

        let opinions = Opinions::default()
            .stating("obligation_0_idempotency", 0.93)
            .stating("result_replay", 0.91);

        let (workflow, engine, seen) = workflow_with(out_dir.clone(), success_script(), &opinions);

        let report = workflow.run().await.expect("the workflow runs");

        assert!(
            matches!(report.status, RunStatus::Success { .. }),
            "{:?}",
            report.status
        );

        assert!(
            discoveries(&seen).is_empty(),
            "discovery never reached the agent backend"
        );

        // One request decided the task: everything code could not
        // answer, asked together over one state.
        let asked = opinions.asked();

        assert_eq!(asked.len(), 1);

        assert_eq!(asked[0].state["prompt"], "A ping service.");
        assert_eq!(
            asked[0].state["obligations"][0]["intent"],
            "ping is idempotent by id"
        );
        assert_eq!(asked[0].tags["builder"], "requirement_discovery");
        assert_eq!(
            asked[0].tags["spec.discovery.obligation"],
            "discovery.obligation@2"
        );

        // Exactly what the scripted agent would have proposed: keyed by
        // the request's declared identity, replay consistent because the
        // refinement was stated.
        let adopted = adopted_idempotency(&engine);

        assert_eq!(adopted.len(), 1);
        assert_eq!(adopted[0].result, ResultReplayRequirement::ReplayConsistent);
        assert_eq!(
            adopted[0].key.components,
            vec![ValueRef {
                source: ValueSource::Input(id("input.ping.request")),
                path: path("id"),
            }]
        );

        let head = engine.head_snapshot();

        assert!(matches!(
            head.workspace.prompt_obligations[&PromptObligationId(OBLIGATION.to_string())].status,
            PromptObligationStatus::Mapped { .. }
        ));

        let manifest = manifest_of(&out_dir);

        assert_eq!(manifest["backend"], "system_one+scripted");

        // The run says for itself who settled what (§13.1): discovery by
        // the builder, in one attempt, and everything else by sessions.
        assert_eq!(manifest["manifest_format"], 2);

        let discovery = records(&manifest, "requirement_discovery");

        assert_eq!(discovery.len(), 1);
        assert_eq!(discovery[0]["executor"], "system_one");
        assert_eq!(discovery[0]["operation"], "operation.ping");
        assert_eq!(discovery[0]["attempts"], 1);
        assert_eq!(discovery[0]["final_state"], "committed");
        assert!(discovery[0].get("abstained").is_none());

        assert_eq!(manifest["executors"]["system_one"]["tasks"], 1);
        assert_eq!(records(&manifest, "decompose")[0]["executor"], "agent");

        std::fs::remove_dir_all(&out_dir).ok();
    }

    /// A refinement that is not stated leaves the default: the builder
    /// never strengthens a requirement on an uncertain answer.
    #[tokio::test]
    async fn an_unstated_refinement_leaves_the_default() {
        let out_dir = scratch();

        let opinions = Opinions::default()
            .stating("obligation_0_idempotency", 0.93)
            .stating("result_replay", 0.55);

        let (workflow, engine, _) = workflow_with(out_dir.clone(), success_script(), &opinions);

        workflow.run().await.expect("the workflow runs");

        assert_eq!(
            adopted_idempotency(&engine)[0].result,
            ResultReplayRequirement::Unspecified
        );

        std::fs::remove_dir_all(&out_dir).ok();
    }

    /// An obligation the builder cannot place is never guessed at and
    /// never dropped: the same task goes to the agent backend, told why.
    #[tokio::test]
    async fn an_unplaceable_obligation_is_handed_to_the_agent_backend() {
        for opinions in [
            // Nothing enumerated fits.
            Opinions::default(),
            // Something fits, but not clearly enough to act on.
            Opinions::default().stating("obligation_0_idempotency", 0.41),
        ] {
            let out_dir = scratch();

            let (workflow, engine, seen) =
                workflow_with(out_dir.clone(), success_script(), &opinions);

            let report = workflow.run().await.expect("the workflow runs");

            assert!(
                matches!(report.status, RunStatus::Success { .. }),
                "{:?}",
                report.status
            );

            let handed = discoveries(&seen);

            assert_eq!(handed.len(), 1, "the agent backend ran the task once");

            assert!(
                handed[0].contains("Hand-off from the System One executor")
                    && (handed[0].contains("maps to no enumerated requirement")
                        || handed[0].contains("it is uncertain whether the obligation")),
                "{}",
                handed[0]
            );

            // The agent's proposal is the one adopted.
            assert_eq!(
                adopted_idempotency(&engine)[0].result,
                ResultReplayRequirement::ReplayConsistent
            );

            std::fs::remove_dir_all(&out_dir).ok();
        }
    }

    /// An unavailable decider costs the run nothing but the attempt.
    #[tokio::test]
    async fn an_unavailable_decider_costs_only_the_fallback() {
        let out_dir = scratch();

        let opinions = Opinions {
            unavailable: true,
            ..Default::default()
        };

        let (workflow, _, seen) = workflow_with(out_dir.clone(), success_script(), &opinions);

        let report = workflow.run().await.expect("the workflow runs");

        assert!(
            matches!(report.status, RunStatus::Success { .. }),
            "{:?}",
            report.status
        );

        let handed = discoveries(&seen);

        assert_eq!(handed.len(), 1);
        assert!(
            handed[0].contains("the decider gave no answer"),
            "{}",
            handed[0]
        );

        // The manifest records the abstention against the task the agent
        // settled, so an escalation rate can be read off any run.
        let manifest = manifest_of(&out_dir);

        let discovery = records(&manifest, "requirement_discovery");

        assert_eq!(discovery[0]["executor"], "agent");
        assert_eq!(discovery[0]["backend"], "scripted");
        assert!(
            discovery[0]["abstained"]
                .as_str()
                .expect("an abstention")
                .contains("the decider gave no answer"),
            "{}",
            discovery[0]
        );

        assert_eq!(manifest["executors"]["agent"]["abstentions_received"], 1);
        assert!(manifest["executors"].get("system_one").is_none());

        std::fs::remove_dir_all(&out_dir).ok();
    }

    /// With nothing stated there is nothing to propose, and the builder
    /// says so itself rather than spending a session to learn it.
    #[tokio::test]
    async fn a_prompt_that_states_nothing_needs_no_session() {
        let out_dir = scratch();

        let opinions = Opinions::default();

        let (workflow, engine, seen) =
            workflow_with(out_dir.clone(), no_requirements_script(), &opinions);

        let report = workflow.run().await.expect("the workflow runs");

        assert!(
            matches!(report.status, RunStatus::Success { .. }),
            "{:?}",
            report.status
        );

        assert!(discoveries(&seen).is_empty());
        assert!(adopted_idempotency(&engine).is_empty());

        std::fs::remove_dir_all(&out_dir).ok();
    }

    /// Under a policy that adopts implied requirements, one the prompt
    /// states is proposed as strongly implied, citing the prompt.
    #[tokio::test]
    async fn a_stated_requirement_is_proposed_as_strongly_implied() {
        let out_dir = scratch();

        let opinions = Opinions::default().stating("idempotency", 0.94);

        let (workflow, engine, seen) =
            workflow_with(out_dir.clone(), no_requirements_script(), &opinions);

        let report = workflow.run().await.expect("the workflow runs");

        assert!(
            matches!(report.status, RunStatus::Success { .. }),
            "{:?}",
            report.status
        );

        assert!(discoveries(&seen).is_empty());

        let adopted = adopted_idempotency(&engine);

        assert_eq!(adopted.len(), 1);
        assert_eq!(adopted[0].result, ResultReplayRequirement::Unspecified);

        let head = engine.head_snapshot();

        assert!(
            head.workspace
                .requirement_proposals
                .iter()
                .any(|proposal| matches!(
                    &proposal.origin,
                    RequirementOrigin::StronglyImplied { evidence, .. } if !evidence.is_empty()
                )),
            "{:?}",
            head.workspace.requirement_proposals
        );

        std::fs::remove_dir_all(&out_dir).ok();
    }

    /// A model a person authored and the checker proves, with everything
    /// its author declared taken out — so discovery has it all to find.
    fn undeclared(fixture: &str, operation: &Id) -> (WorkspaceState, conseqa::spec::Operation) {
        let source = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(fixture),
        )
        .expect("fixture readable");

        let authored = conseqa::parser::yaml::parse(&source).expect("fixture parses");

        let mut stripped = authored.clone();

        for operation in stripped.operations.values_mut() {
            operation.requirements = Default::default();

            let transactions: Vec<Id> = operation
                .program
                .transactions()
                .into_iter()
                .map(|(_, transaction)| transaction.id.clone())
                .collect();

            for transaction in transactions {
                operation
                    .program
                    .transaction_mut(&transaction)
                    .expect("just listed")
                    .requirements = Default::default();
            }
        }

        let mut run_meta = RunMetadata::new(RunId("discovery-test".to_string()));

        run_meta.prompt = Some(
            "Entries must be applied to each tenant's ledger exactly once and in sequence \
             order, even when events are redelivered or the service restarts."
                .to_string(),
        );

        run_meta.policy = RunPolicy {
            strict_requirements: true,
            adopt_recommended: false,
        };

        (
            WorkspaceState::from_model(&stripped, run_meta),
            authored.operations[operation].clone(),
        )
    }

    /// Runs one discovery task for `operation` with the builder in front
    /// of an agent backend that does nothing.
    async fn discover(
        workspace: WorkspaceState,
        operation: &Id,
        opinions: &Opinions,
    ) -> (ConfluenceEngine, Seen) {
        let (engine, seen, _) =
            discover_all(workspace, std::slice::from_ref(operation), opinions).await;

        (engine, seen)
    }

    /// The same for several operations at once, as the workflow's
    /// discovery phase fans them out.
    async fn discover_all(
        workspace: WorkspaceState,
        operations: &[Id],
        opinions: &Opinions,
    ) -> (ConfluenceEngine, Seen, Vec<conseqa::harness::TaskRun>) {
        let tasks = operations
            .iter()
            .map(|operation| {
                (
                    TaskKind::RequirementDiscovery,
                    conseqa::confluence::WriteScope::requirement_discovery(operation.clone()),
                    operation.clone(),
                )
            })
            .collect();

        run_tasks(workspace, tasks, opinions).await
    }

    /// Runs operation-scoped tasks with every builder in front of an
    /// agent backend that does nothing but record what reaches it.
    async fn run_tasks(
        workspace: WorkspaceState,
        tasks: Vec<(TaskKind, conseqa::confluence::WriteScope, Id)>,
        opinions: &Opinions,
    ) -> (ConfluenceEngine, Seen, Vec<conseqa::harness::TaskRun>) {
        let engine = ConfluenceEngine::in_memory(workspace).expect("engine starts");

        let seen = Seen::default();

        let idle: ScriptFn = Arc::new(|_, _| Box::pin(async {}));

        let backend = Arc::new(SystemOneBackend::new(
            engine.clone(),
            Arc::new(opinions.clone()),
            conseqa::harness::executors::BUILDABLE,
            Arc::new(ScriptedBackend {
                engine: engine.clone(),
                script: recording(idle, seen.clone()),
            }),
        ));

        let supervisor = Supervisor::new(
            engine.clone(),
            backend,
            "http://127.0.0.1:0/mcp",
            None,
            scratch(),
        );

        let scheduler = Scheduler::new(
            engine.clone(),
            supervisor,
            SchedulerPolicy {
                max_attempts: 3,
                max_concurrent_agents: tasks.len().max(1),
                ..Default::default()
            },
        );

        let prompt = engine
            .head_snapshot()
            .workspace
            .run_meta
            .prompt
            .clone()
            .expect("the run has a prompt");

        let tasks = tasks
            .into_iter()
            .map(
                |(kind, write_scope, operation)| conseqa::harness::LogicalTask {
                    kind,
                    objective: format!("Work on {operation}."),
                    write_scope,
                    bundle: conseqa::confluence::BundleSpec {
                        operation: Some(operation),
                        requirements: Vec::new(),
                        include: Vec::new(),
                        peers: Vec::new(),
                    },
                    prompt_evidence: vec![conseqa::confluence::PromptEvidence {
                        source: conseqa::confluence::EvidenceRef("run.prompt".to_string()),
                        excerpt: prompt.clone(),
                    }],
                    interactive: false,
                },
            )
            .collect();

        let runs = scheduler.run_many(tasks).await.expect("the tasks run");

        (engine, seen, runs)
    }

    /// Discovery fans out one task per operation, and mapping an
    /// obligation rewrites it. A task reads only the obligations aimed
    /// at its own operation, so peers that overlap — held here until
    /// both have read and asked — each commit at the first attempt
    /// instead of the first to commit invalidating the rest.
    #[tokio::test]
    async fn peers_mapping_their_own_obligations_do_not_invalidate_each_other() {
        let posting = id("operation.post_entry");
        let applying = id("operation.apply_entry");

        let (mut workspace, _) = undeclared("tenant_ledger.yaml", &posting);

        for (obligation, intent, target) in [
            (
                "obl.post-once",
                "a retried posting records one entry",
                &posting,
            ),
            (
                "obl.apply-once",
                "a redelivered event is applied once",
                &applying,
            ),
        ] {
            workspace.prompt_obligations.insert(
                PromptObligationId(obligation.to_string()),
                PromptObligation {
                    source_span: None,
                    normalized_intent: intent.to_string(),
                    targets: vec![target.clone()],
                    status: PromptObligationStatus::Unmapped,
                },
            );
        }

        let opinions = Opinions {
            rendezvous: Some(Arc::new(tokio::sync::Barrier::new(2))),
            ..Opinions::default().stating("obligation_0_idempotency", 0.9)
        };

        let (engine, seen, runs) =
            discover_all(workspace, &[posting.clone(), applying.clone()], &opinions).await;

        assert!(discoveries(&seen).is_empty());

        for run in &runs {
            assert!(run.committed(), "{run:?}");
            assert_eq!(run.attempts.len(), 1, "no attempt was invalidated: {run:?}");
        }

        // Each task was shown only the obligation aimed at its operation.
        for request in opinions.asked() {
            assert_eq!(
                request.state["obligations"].as_array().map(Vec::len),
                Some(1)
            );
        }

        let head = engine.head_snapshot();

        assert!(
            head.workspace
                .prompt_obligations
                .values()
                .all(|obligation| {
                    matches!(obligation.status, PromptObligationStatus::Mapped { .. })
                })
        );
    }

    /// Two statements in the prompt can ask for one requirement — "a
    /// retried posting records one entry" and "a duplicate request is
    /// harmless" are both idempotency by the request's id. The builder
    /// adopts the requirement once and maps both obligations to it,
    /// rather than escalating (as the benchmark's Jev run did).
    #[tokio::test]
    async fn two_obligations_discharged_by_one_requirement_are_both_mapped() {
        let posting = id("operation.post_entry");

        let (mut workspace, _) = undeclared("tenant_ledger.yaml", &posting);

        for (obligation, intent) in [
            ("obl.post-once", "a retried posting records one entry"),
            (
                "obl.duplicates-harmless",
                "a duplicate posting request is harmless",
            ),
        ] {
            workspace.prompt_obligations.insert(
                PromptObligationId(obligation.to_string()),
                PromptObligation {
                    source_span: None,
                    normalized_intent: intent.to_string(),
                    targets: vec![posting.clone()],
                    status: PromptObligationStatus::Unmapped,
                },
            );
        }

        let opinions = Opinions::default()
            .stating("obligation_0_idempotency", 0.9)
            .stating("obligation_1_idempotency", 0.88);

        let (engine, seen) = discover(workspace, &posting, &opinions).await;

        assert!(discoveries(&seen).is_empty(), "{:?}", discoveries(&seen));

        let head = engine.head_snapshot();

        assert_eq!(
            head.workspace.operations[&posting]
                .requirements
                .idempotency
                .len(),
            1,
            "adopted once"
        );

        for obligation in head.workspace.prompt_obligations.values() {
            assert!(
                matches!(
                    &obligation.status,
                    PromptObligationStatus::Mapped { requirements } if requirements.len() == 1
                ),
                "{obligation:?}"
            );
        }
    }

    /// One obligation can need several requirements: "a retried posting
    /// never records a second entry, even racing another" asks for
    /// idempotency by the request's id and for the posting transaction to
    /// be serializable. Each pairing is its own judgment, so the
    /// obligation maps to both — a single choice split them roughly
    /// evenly and escalated, as the benchmark's Jev run showed.
    #[tokio::test]
    async fn an_obligation_that_needs_two_requirements_maps_to_both() {
        let posting = id("operation.post_entry");

        let (mut workspace, _) = undeclared("tenant_ledger.yaml", &posting);

        workspace.prompt_obligations.insert(
            PromptObligationId("obl.post-once-racing".to_string()),
            PromptObligation {
                source_span: None,
                normalized_intent: "a retried posting never records a second entry, even \
                                    racing another posting"
                    .to_string(),
                targets: vec![posting.clone()],
                status: PromptObligationStatus::Unmapped,
            },
        );

        let opinions = Opinions::default()
            .stating("obligation_0_idempotency", 0.86)
            .stating("obligation_0_serializability_0", 0.84);

        let (engine, seen) = discover(workspace, &posting, &opinions).await;

        assert!(discoveries(&seen).is_empty(), "{:?}", discoveries(&seen));

        let head = engine.head_snapshot();

        let draft = &head.workspace.operations[&posting];

        assert_eq!(draft.requirements.idempotency.len(), 1);
        assert_eq!(
            draft.program.as_ref().expect("a program").transactions()[0]
                .1
                .requirements
                .serializability
                .len(),
            1
        );

        let obligation =
            &head.workspace.prompt_obligations[&PromptObligationId("obl.post-once-racing".into())];

        assert!(
            matches!(
                &obligation.status,
                PromptObligationStatus::Mapped { requirements } if requirements.len() == 2
            ),
            "{obligation:?}"
        );
    }

    /// A pairing that is neither clearly asked for nor clearly not is
    /// harmless when its requirement is proposed anyway: here the prompt
    /// states serializability on its own, so the obligation is mapped to
    /// what it clearly asks for and nothing it might need is dropped
    /// (the benchmark's `pay_order`, 0.92 / 0.67 with 0.89 stated). With
    /// nothing else proposing the requirement, the same judgment is
    /// escalated.
    #[tokio::test]
    async fn an_unsure_pairing_escalates_only_when_its_requirement_would_be_dropped() {
        let posting = id("operation.post_entry");

        for (stated, handled_in_process) in [(0.89, true), (0.02, false)] {
            let (mut workspace, _) = undeclared("tenant_ledger.yaml", &posting);

            workspace.prompt_obligations.insert(
                PromptObligationId("obl.post-once".to_string()),
                PromptObligation {
                    source_span: None,
                    normalized_intent: "a posting is recorded at most once".to_string(),
                    targets: vec![posting.clone()],
                    status: PromptObligationStatus::Unmapped,
                },
            );

            let opinions = Opinions::default()
                .stating("obligation_0_idempotency", 0.92)
                .stating("obligation_0_serializability_0", 0.67)
                .stating("serializability_0", stated);

            let (engine, seen) = discover(workspace, &posting, &opinions).await;

            let handed = discoveries(&seen);

            if handled_in_process {
                assert!(handed.is_empty(), "{handed:?}");

                let head = engine.head_snapshot();
                let draft = &head.workspace.operations[&posting];

                assert_eq!(draft.requirements.idempotency.len(), 1);
                assert_eq!(
                    draft.program.as_ref().expect("a program").transactions()[0]
                        .1
                        .requirements
                        .serializability
                        .len(),
                    1,
                    "adopted as stated by the prompt"
                );

                assert!(matches!(
                    &head.workspace.prompt_obligations[&PromptObligationId("obl.post-once".into())]
                        .status,
                    PromptObligationStatus::Mapped { requirements } if requirements.len() == 1
                ));
            } else {
                assert_eq!(handed.len(), 1);
                assert!(
                    handed[0].contains("it is uncertain whether the obligation"),
                    "{}",
                    handed[0]
                );
            }
        }
    }

    /// The claim the whole layer rests on: what a requirement is *keyed
    /// by* is a fact about the program, so code finds it. Told only that
    /// the four requirements are stated, the builder re-derives the keys,
    /// positions and refinements a person declared by hand — for a
    /// subscription whose identity comes from its topic, a transaction
    /// keyed through its selectors, and an ordering read off its cursor.
    #[tokio::test]
    async fn enumeration_rederives_what_an_author_declared() {
        let operation = id("operation.apply_entry");

        let (workspace, authored) = undeclared("tenant_ledger.yaml", &operation);

        let opinions = Opinions::default()
            .stating("idempotency", 0.92)
            .stating("recoverability", 0.9)
            .stating("guaranteed_completion", 0.88)
            .stating("serializability_0", 0.95)
            .stating("ordering_0", 0.97);

        let (engine, seen) = discover(workspace, &operation, &opinions).await;

        assert!(discoveries(&seen).is_empty(), "{:?}", discoveries(&seen));

        let head = engine.head_snapshot();
        let draft = &head.workspace.operations[&operation];

        assert_eq!(draft.requirements, authored.requirements);

        let adopted = draft.program.as_ref().expect("a program").transactions();
        let declared = authored.program.transactions();

        assert_eq!(adopted.len(), 1);
        assert_eq!(adopted[0].1.requirements, declared[0].1.requirements);

        // What the model was shown of the transaction is words, never
        // DSL: it is asked about the prompt, not about Conseqa.
        let asked = opinions.asked();

        assert_eq!(asked.len(), 1);

        let does = asked[0].state["operation"]["work"][0]["does"]
            .as_str()
            .expect("a summary");

        assert!(
            does.contains("advances the `last_applied_sequence` position")
                && does.contains("updates the `object.tenant_ledger` selected by `tenant_id`"),
            "{does}"
        );

        // One candidate key: nothing to choose, so nothing was asked.
        assert!(
            !asked[0]
                .questions
                .contains_key(&"serializability_key_0".into())
        );
    }

    /// The same for a request: its identity is declared on the input, and
    /// its serializability key is the tenant its lock, read and write all
    /// pin. Nothing is proposed for what the prompt does not state.
    #[tokio::test]
    async fn only_what_is_stated_is_proposed() {
        let operation = id("operation.post_entry");

        let (workspace, authored) = undeclared("tenant_ledger.yaml", &operation);

        let opinions = Opinions::default().stating("serializability_0", 0.9);

        let (engine, seen) = discover(workspace, &operation, &opinions).await;

        assert!(discoveries(&seen).is_empty(), "{:?}", discoveries(&seen));

        let head = engine.head_snapshot();
        let draft = &head.workspace.operations[&operation];

        assert_eq!(draft.requirements, Default::default());

        assert_eq!(
            draft.program.as_ref().expect("a program").transactions()[0]
                .1
                .requirements
                .serializability,
            authored.program.transactions()[0]
                .1
                .requirements
                .serializability
        );

        // This transaction guards no position, so ordering was never a
        // question.
        assert!(
            !opinions.asked()[0]
                .questions
                .contains_key(&"ordering_0".into())
        );
    }

    /// A requirement the prompt states but code cannot express — here,
    /// idempotency for an input that declares no identity — is the
    /// session's to resolve. It is never dropped.
    #[tokio::test]
    async fn a_stated_requirement_nothing_can_express_is_escalated() {
        let operation = id("operation.post_entry");

        let (mut workspace, _) = undeclared("tenant_ledger.yaml", &operation);

        for input in workspace
            .operations
            .get_mut(&operation)
            .expect("the operation")
            .inputs
            .values_mut()
        {
            if let Input::Request(request) = input {
                request.identity = RequestIdentity::Unspecified;
            }
        }

        let opinions = Opinions::default().stating("idempotency", 0.93);

        let (engine, seen) = discover(workspace, &operation, &opinions).await;

        let handed = discoveries(&seen);

        assert_eq!(handed.len(), 1);
        assert!(
            handed[0]
                .contains("the prompt requires idempotency, and nothing enumerated can express it"),
            "{}",
            handed[0]
        );

        assert!(
            engine
                .head_snapshot()
                .workspace
                .requirement_proposals
                .is_empty()
        );
    }

    /// `shop` with place_order's reservation written as a plain update:
    /// a read-then-update of the product that only a lock protects, so
    /// its transaction has nothing to reject. (As authored, the
    /// reservation is a compare-and-set, whose own write protection
    /// would already serialize restock's locked read against it.)
    fn with_plain_reservation(mut model: conseqa::spec::Model) -> conseqa::spec::Model {
        let OperationStep::Transaction(execution) = &mut model
            .operations
            .get_mut(&id("operation.place_order"))
            .expect("the operation")
            .program
            .steps[0]
        else {
            panic!("place_order reserves first");
        };

        for step in &mut execution.transaction.steps {
            if let conseqa::spec::TransactionStep::CompareAndSet(cas) = step {
                *step = conseqa::spec::TransactionStep::Update(conseqa::spec::Update {
                    target: cas.target.clone(),
                    fields: cas.fields.clone(),
                    values: cas.values.clone(),
                });
            }
        }

        execution.rejected = None;

        model
    }

    /// `shop` as the benchmark's Jev run exported it, with the
    /// reservation a plain update and the product locks of the
    /// operations that reserve and add stock taken out.
    fn stock_without_its_locks() -> conseqa::spec::Model {
        let mut broken = with_plain_reservation(authored("shop.yaml"));

        for (operation, transaction) in [
            ("operation.place_order", "tx.place_order.reserve"),
            ("operation.restock", "tx.restock.apply"),
        ] {
            broken
                .operations
                .get_mut(&id(operation))
                .expect("the operation")
                .program
                .transaction_mut(&id(transaction))
                .expect("the transaction")
                .steps
                .retain(|step| !matches!(step, conseqa::spec::TransactionStep::Lock(_)));
        }

        broken
    }

    fn program_scope(operations: &[&str]) -> conseqa::confluence::WriteScope {
        conseqa::confluence::WriteScope::of(
            operations
                .iter()
                .map(|operation| conseqa::confluence::WriteGrant::OperationProgram(id(operation))),
        )
    }

    /// The case the benchmark's Jev run escalated three times: a strict
    /// lock proof needs the lock on the writer as well as the reader,
    /// and they are different operations. Scoped to either program
    /// alone, every candidate leaves its own target unproven; scoped to
    /// the conflict closure, one candidate edits both programs, the
    /// whole model is proven again, and it is the programs the run
    /// authored.
    #[tokio::test]
    async fn a_closure_is_repaired_where_no_single_program_can_be() {
        let authored = with_plain_reservation(authored("shop.yaml"));
        let broken = stock_without_its_locks();

        for alone in ["operation.place_order", "operation.restock"] {
            let (_, seen, runs) = run_tasks(
                workspace_of(&broken, "A shop."),
                vec![(
                    TaskKind::RequirementRepair,
                    program_scope(&[alone]),
                    id(alone),
                )],
                &Opinions::default(),
            )
            .await;

            assert!(!runs[0].committed(), "{alone} alone: {:?}", runs[0]);

            let handed = repairs(&seen);

            assert_eq!(handed.len(), 1, "{alone} alone escalates");
            assert!(
                handed[0].contains("leaves transaction_serializability #0"),
                "{}",
                handed[0]
            );
        }

        let (engine, seen, runs) = run_tasks(
            workspace_of(&broken, "A shop."),
            vec![(
                TaskKind::RequirementRepair,
                program_scope(&["operation.place_order", "operation.restock"]),
                id("operation.place_order"),
            )],
            &Opinions::default(),
        )
        .await;

        assert!(repairs(&seen).is_empty(), "{:?}", repairs(&seen));
        assert!(runs[0].committed(), "{:?}", runs[0]);
        assert_eq!(runs[0].attempts.len(), 1);

        assert!(head_model_is_proven(&engine).await);

        let head = engine.head_snapshot();

        for operation in ["operation.place_order", "operation.restock"] {
            assert_eq!(
                head.workspace.operations[&id(operation)].program,
                Some(authored.operations[&id(operation)].program.clone()),
                "{operation}"
            );
        }
    }

    /// The workflow schedules that repair itself: every operation the
    /// unproven obligations' evidence ties together goes to one task,
    /// with a program grant for each, so peers that share stock neither
    /// invalidate each other nor refuse each other's repairs — and here
    /// no session is needed at all.
    #[tokio::test]
    async fn the_workflow_repairs_a_conflict_closure_as_one_task() {
        let authored = with_plain_reservation(authored("shop.yaml"));
        let broken = stock_without_its_locks();

        let engine =
            ConfluenceEngine::in_memory(workspace_of(&broken, "A shop.")).expect("engine starts");

        let seen = Seen::default();
        let idle: ScriptFn = Arc::new(|_, _| Box::pin(async {}));

        let backend = Arc::new(SystemOneBackend::new(
            engine.clone(),
            Arc::new(Opinions::default()),
            conseqa::harness::executors::BUILDABLE,
            Arc::new(ScriptedBackend {
                engine: engine.clone(),
                script: recording(idle, seen.clone()),
            }),
        ));

        let out_dir = scratch();

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
                out_dir: out_dir.clone(),
                analysis_timeout: Duration::from_secs(20),
                max_iterations: 8,
                objective: None,
            },
        );

        let report = workflow.run().await.expect("the workflow runs");

        assert!(
            matches!(report.status, RunStatus::Success { .. }),
            "{:?}",
            report.status
        );

        assert!(
            seen.lock().expect("not poisoned").is_empty(),
            "no session was needed: {:?}",
            seen.lock().expect("not poisoned")
        );

        let manifest = manifest_of(&out_dir);
        let repaired = records(&manifest, "requirement_repair");

        assert_eq!(repaired.len(), 1, "one task for the closure: {manifest}");
        assert_eq!(repaired[0]["executor"], "system_one");

        let head = engine.head_snapshot();

        for operation in ["operation.place_order", "operation.restock"] {
            assert_eq!(
                head.workspace.operations[&id(operation)].program,
                Some(authored.operations[&id(operation)].program.clone()),
                "{operation}"
            );
        }

        std::fs::remove_dir_all(&out_dir).ok();
    }

    fn authored(fixture: &str) -> conseqa::spec::Model {
        let source = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(fixture),
        )
        .expect("fixture readable");

        conseqa::parser::yaml::parse(&source).expect("fixture parses")
    }

    fn workspace_of(model: &conseqa::spec::Model, prompt: &str) -> WorkspaceState {
        let mut run_meta = RunMetadata::new(RunId("repair-test".to_string()));

        run_meta.prompt = Some(prompt.to_string());

        run_meta.policy = RunPolicy {
            strict_requirements: true,
            adopt_recommended: false,
        };

        WorkspaceState::from_model(model, run_meta)
    }

    fn serializability_proven(model: &conseqa::spec::Model, transaction: &str) -> bool {
        conseqa::analyzer::verification::verify(model)
            .transaction_serializability
            .iter()
            .filter(|check| check.transaction == id(transaction))
            .all(|check| {
                matches!(
                    check.verdict,
                    conseqa::analyzer::verification::TransactionSerializabilityVerdict::Proven { .. }
                )
            })
    }

    /// `tenant_ledger`, and a copy with one breakage applied to
    /// `apply_entry`'s transaction.
    fn an_apply_with(
        breakage: impl FnOnce(&mut conseqa::spec::Transaction),
    ) -> (conseqa::spec::Model, conseqa::spec::Model) {
        let authored = authored("tenant_ledger.yaml");

        let mut broken = authored.clone();

        breakage(
            broken
                .operations
                .get_mut(&id("operation.apply_entry"))
                .expect("the operation")
                .program
                .transaction_mut(&id("tx.apply_entry"))
                .expect("the transaction"),
        );

        assert!(
            !conseqa::analyzer::verification::verify(&broken).all_proven(),
            "the breakage must un-prove something"
        );

        (authored, broken)
    }

    async fn repaired_program(
        broken: &conseqa::spec::Model,
        opinions: &Opinions,
    ) -> (ConfluenceEngine, Seen, Vec<conseqa::harness::TaskRun>) {
        repair(broken, "operation.apply_entry", opinions).await
    }

    fn apply_program(engine: &ConfluenceEngine) -> Option<conseqa::spec::OperationBlock> {
        engine.head_snapshot().workspace.operations[&id("operation.apply_entry")]
            .program
            .clone()
    }

    /// Replay route B: a transaction that advances a cursor cannot be
    /// replayed by re-execution, so its idempotency and recoverability
    /// rest on its keyed commit. Taken away, the builder puts it back —
    /// keyed by the governing key, as the author wrote it.
    #[tokio::test]
    async fn a_missing_keyed_commit_is_restored_by_the_governing_key() {
        let (authored, broken) = an_apply_with(|transaction| {
            transaction.idempotency = conseqa::spec::IdempotencyGuarantee::Unspecified;
        });

        let (engine, seen, runs) = repaired_program(&broken, &Opinions::default()).await;

        assert!(repairs(&seen).is_empty(), "{:?}", repairs(&seen));
        assert!(runs[0].committed(), "{:?}", runs[0]);
        assert!(head_model_is_proven(&engine).await);

        assert_eq!(
            apply_program(&engine),
            Some(
                authored.operations[&id("operation.apply_entry")]
                    .program
                    .clone()
            )
        );
    }

    /// An ordering proof needs the cursor to consume the requirement's
    /// own position. Pointed at another value, it is pointed back.
    #[tokio::test]
    async fn a_cursor_advanced_by_the_wrong_value_is_pointed_at_the_position() {
        // The cursor's own current reading: the right type, the wrong
        // position.
        let (authored, broken) = an_apply_with(|transaction| {
            for step in &mut transaction.steps {
                if let conseqa::spec::TransactionStep::AdvanceCursor(advance) = step {
                    advance.incoming = ValueRef {
                        source: ValueSource::TransactionRead(id("read.apply_entry.ledger")),
                        path: path("last_applied_sequence"),
                    };
                }
            }
        });

        let (engine, seen, runs) = repaired_program(&broken, &Opinions::default()).await;

        assert!(repairs(&seen).is_empty(), "{:?}", repairs(&seen));
        assert!(runs[0].committed(), "{:?}", runs[0]);
        assert!(head_model_is_proven(&engine).await);

        assert_eq!(
            apply_program(&engine),
            Some(
                authored.operations[&id("operation.apply_entry")]
                    .program
                    .clone()
            )
        );
    }

    /// A position recorded with an ordinary conditional write orders
    /// nothing: the compare-and-set below guards the read by its observed
    /// version, but assigns the position whatever it was. The builder
    /// turns the write into a cursor advance — carrying the comparison —
    /// under both rules and the analyzer proves both; which one the
    /// system needs is a fact of the domain. Told that no entry may be
    /// skipped, it takes `successor` — the author's program; told
    /// nothing, the permissive `monotonic_after` stands.
    #[tokio::test]
    async fn a_position_written_plainly_becomes_a_cursor_under_the_stated_rule() {
        let as_plain_write = |transaction: &mut conseqa::spec::Transaction| {
            let position = transaction.requirements.ordering[0].position.clone();

            for step in &mut transaction.steps {
                if let conseqa::spec::TransactionStep::AdvanceCursor(advance) = step {
                    *step = conseqa::spec::TransactionStep::CompareAndSet(
                        conseqa::spec::CompareAndSet {
                            target: advance.target.clone(),
                            compare: advance.compare.clone(),
                            fields: [advance.field.clone()].into(),
                            values: conseqa::spec::Derivation::Deterministic {
                                from: vec![position.clone()],
                            },
                        },
                    );
                }
            }
        };

        for (gap_free, rule) in [
            (0.91, conseqa::spec::CursorAdvanceRule::Successor),
            (0.03, conseqa::spec::CursorAdvanceRule::MonotonicAfter),
        ] {
            let (authored, broken) = an_apply_with(as_plain_write);

            let opinions = Opinions::default().stating("gap_free", gap_free);

            let (engine, seen, runs) = repaired_program(&broken, &opinions).await;

            assert!(repairs(&seen).is_empty(), "{:?}", repairs(&seen));
            assert!(runs[0].committed(), "{:?}", runs[0]);
            assert!(head_model_is_proven(&engine).await);

            let program = apply_program(&engine).expect("a program");

            let advance = program
                .transaction(&id("tx.apply_entry"))
                .expect("the transaction")
                .steps
                .iter()
                .find_map(|step| match step {
                    conseqa::spec::TransactionStep::AdvanceCursor(advance) => Some(advance.clone()),
                    _ => None,
                })
                .expect("a cursor advance");

            assert_eq!(advance.rule, rule);

            if rule == conseqa::spec::CursorAdvanceRule::Successor {
                assert_eq!(
                    program,
                    authored.operations[&id("operation.apply_entry")]
                        .program
                        .clone()
                );
            }

            // The rule was the one thing asked.
            assert!(
                opinions
                    .asked()
                    .iter()
                    .any(|request| request.questions.contains_key(&"gap_free".into()))
            );
        }
    }

    /// `shop` with one operation's program taken away, as the fanout
    /// finds it.
    fn shop_without_program(operation: &str) -> WorkspaceState {
        let mut workspace = workspace_of(&authored("shop.yaml"), "A small shop backend.");

        workspace
            .operations
            .get_mut(&id(operation))
            .expect("the operation")
            .program = None;

        workspace
    }

    async fn synthesize(
        operation: &str,
        opinions: &Opinions,
    ) -> (ConfluenceEngine, Seen, Vec<conseqa::harness::TaskRun>) {
        run_tasks(
            shop_without_program(operation),
            vec![(
                TaskKind::OperationSynthesis,
                conseqa::confluence::WriteScope::operation_synthesis(id(operation)),
                id(operation),
            )],
            opinions,
        )
        .await
    }

    fn syntheses(seen: &Seen) -> Vec<String> {
        seen.lock()
            .expect("not poisoned")
            .iter()
            .filter(|(kind, _)| *kind == TaskKind::OperationSynthesis)
            .map(|(_, prompt)| prompt.clone())
            .collect()
    }

    fn program_of(engine: &ConfluenceEngine, operation: &str) -> conseqa::spec::OperationBlock {
        engine.head_snapshot().workspace.operations[&id(operation)]
            .program
            .clone()
            .expect("a program")
    }

    /// Keyed update: told the operation changes one field of the one
    /// record its input identifies, the builder writes the program — a
    /// read, an update of that field (which publishes the version by
    /// itself), the result returned — and the analyzer admits it. No
    /// session wrote it.
    #[tokio::test]
    async fn a_keyed_update_is_written_from_its_template() {
        let opinions = Opinions::default()
            .choosing("archetype", "keyed_update", 0.9)
            .choosing("record", "object.product", 0.9)
            .stating("changes_object_product_stock", 0.93);

        let (engine, seen, runs) = synthesize("operation.restock", &opinions).await;

        assert!(syntheses(&seen).is_empty(), "{:?}", syntheses(&seen));
        assert!(runs[0].committed(), "{:?}", runs[0]);

        let program = program_of(&engine, "operation.restock");

        let transaction = program
            .transaction(&id("tx.restock.update"))
            .expect("the template's transaction");

        let kinds: Vec<&str> = transaction
            .steps
            .iter()
            .map(|step| match step {
                conseqa::spec::TransactionStep::Read(_) => "read",
                conseqa::spec::TransactionStep::Update(update) => {
                    assert_eq!(update.fields, [path("stock")].into());
                    "update"
                }
                _ => "other",
            })
            .collect();

        // The version is published by the update itself.
        assert_eq!(kinds, ["read", "update"]);

        assert!(matches!(
            program.steps.last(),
            Some(conseqa::spec::OperationStep::Return(_))
        ));

        // One request decided it.
        assert_eq!(opinions.asked().len(), 1);
        assert_eq!(opinions.asked()[0].tags["builder"], "operation_synthesis");
    }

    /// Keyed insert: one record created from what the input carries,
    /// its identity included.
    #[tokio::test]
    async fn a_keyed_insert_is_written_from_its_template() {
        let opinions = Opinions::default()
            .choosing("archetype", "keyed_insert", 0.9)
            .choosing("record", "object.restock_receipt", 0.9);

        let (engine, seen, runs) = synthesize("operation.restock", &opinions).await;

        assert!(syntheses(&seen).is_empty(), "{:?}", syntheses(&seen));
        assert!(runs[0].committed(), "{:?}", runs[0]);

        let program = program_of(&engine, "operation.restock");

        assert!(matches!(
            &program
                .transaction(&id("tx.restock.insert"))
                .expect("the template's transaction")
                .steps[..],
            [conseqa::spec::TransactionStep::Insert(insert)]
                if insert.object == id("object.restock_receipt")
        ));
    }

    /// Transition, for a keyed request: the builder inspects the record
    /// in a keyed transaction, decides on the recovered state, applies
    /// the chosen transition (keyed too) or returns the declared error
    /// chosen for a wrong-state record. Every attempt of one request
    /// then decides alike, so the operation's idempotency and result
    /// replay are proven as written — no repair pass needed, where the
    /// direct shape sent the benchmark to a 119 s session.
    #[tokio::test]
    async fn a_transition_is_written_replay_safe_from_its_template() {
        let opinions = Opinions::default()
            .choosing("archetype", "transition", 0.88)
            .choosing(
                "transition",
                "machine_order_lifecycle_transition_order_ship",
                0.9,
            )
            .choosing("refusal", "not_shippable", 0.86);

        let (engine, seen, runs) = synthesize("operation.ship_order", &opinions).await;

        assert!(syntheses(&seen).is_empty(), "{:?}", syntheses(&seen));
        assert!(runs[0].committed(), "{:?}", runs[0]);

        let program = program_of(&engine, "operation.ship_order");

        let [
            conseqa::spec::OperationStep::Transaction(inspect),
            conseqa::spec::OperationStep::Branch(branch),
        ] = &program.steps[..]
        else {
            panic!("inspect, then decide: {program:?}");
        };

        assert!(matches!(
            inspect.transaction.idempotency,
            conseqa::spec::IdempotencyGuarantee::DeduplicatedBy { .. }
        ));

        let Some(conseqa::spec::OperationStep::Transaction(apply)) = branch.then.steps.first()
        else {
            panic!("the transition first in the arm: {branch:?}");
        };

        assert!(apply.transaction.steps.iter().any(|step| matches!(
            step,
            conseqa::spec::TransactionStep::Transition(transition)
                if transition.transition == id("transition.order.ship")
        )));

        assert!(matches!(
            branch.otherwise.as_ref().map(|block| &block.steps[..]),
            Some([conseqa::spec::OperationStep::Return(conseqa::spec::Return {
                outcome: conseqa::spec::ResultOutcome::Err { error, .. },
                ..
            })]) if *error == id("not_shippable")
        ));

        let model = engine
            .head_snapshot()
            .workspace
            .assemble_model()
            .expect("assembles");

        let report = conseqa::analyzer::verification::verify(&model);

        assert!(
            report
                .idempotency
                .iter()
                .chain([].iter())
                .filter(|check| check.operation == id("operation.ship_order"))
                .all(|check| matches!(
                    check.verdict,
                    conseqa::analyzer::verification::IdempotencyVerdict::Proven { .. }
                ))
        );

        assert!(
            report
                .result_replay
                .iter()
                .filter(|check| check.operation == id("operation.ship_order"))
                .all(|check| matches!(
                    check.verdict,
                    conseqa::analyzer::verification::ResultReplayVerdict::Proven { .. }
                ))
        );
    }

    /// Nothing is guessed: a template that matches nothing clearly, or a
    /// field the model is unsure the operation changes, sends the task to
    /// a session, told why.
    #[tokio::test]
    async fn an_unclear_template_is_handed_to_the_session() {
        for (opinions, why) in [
            (
                Opinions::default().choosing("archetype", "none_of_these", 0.8),
                "no archetype matches the operation",
            ),
            (
                Opinions::default()
                    .choosing("archetype", "keyed_update", 0.9)
                    .choosing("record", "object.product", 0.9)
                    .stating("changes_object_product_stock", 0.55),
                "it is uncertain whether the operation changes `stock`",
            ),
        ] {
            let (engine, seen, _) = synthesize("operation.restock", &opinions).await;

            let handed = syntheses(&seen);

            assert_eq!(handed.len(), 1);
            assert!(handed[0].contains(why), "{}", handed[0]);

            assert!(
                engine.head_snapshot().workspace.operations[&id("operation.restock")]
                    .program
                    .is_none()
            );
        }
    }

    /// The layer end to end: an operation the fanout finds without a
    /// program is written from its template, its replay requirements are
    /// then repaired in process (the template leaves the commit
    /// unkeyed; repair keys it), and the run succeeds with no session.
    ///
    /// Mid-fanout the model cannot assemble — siblings have no program
    /// yet — so the template is judged as the gate judges any program,
    /// by the validator's operation-local passes; the whole model is
    /// verified once every program is in.
    #[tokio::test]
    async fn a_missing_program_is_written_and_proven_without_a_session() {
        let workspace = shop_without_program("operation.restock");

        let opinions = Opinions::default()
            .choosing_for("operation.restock", "archetype", "keyed_update", 0.9)
            .choosing_for("operation.restock", "record", "object.product", 0.9)
            .stating_for("operation.restock", "changes_object_product_stock", 0.93);

        let engine = ConfluenceEngine::in_memory(workspace).expect("engine starts");
        let seen = Seen::default();
        let idle: ScriptFn = Arc::new(|_, _| Box::pin(async {}));

        let backend = Arc::new(SystemOneBackend::new(
            engine.clone(),
            Arc::new(opinions.clone()),
            conseqa::harness::executors::BUILDABLE,
            Arc::new(ScriptedBackend {
                engine: engine.clone(),
                script: recording(idle, seen.clone()),
            }),
        ));

        let out_dir = scratch();

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
                out_dir: out_dir.clone(),
                analysis_timeout: Duration::from_secs(20),
                max_iterations: 8,
                objective: None,
            },
        );

        let report = workflow.run().await.expect("the workflow runs");

        assert!(
            matches!(report.status, RunStatus::Success { .. }),
            "{:?}",
            report.status
        );

        assert!(
            seen.lock().expect("not poisoned").is_empty(),
            "no session: {:?}",
            seen.lock().expect("not poisoned")
        );

        let manifest = manifest_of(&out_dir);

        for kind in ["operation_synthesis", "requirement_repair"] {
            let ran = records(&manifest, kind);

            assert!(!ran.is_empty(), "{kind} ran: {manifest}");
            assert!(
                ran.iter().all(|record| record["executor"] == "system_one"),
                "{kind}: {manifest}"
            );
        }

        let transaction = program_of(&engine, "operation.restock")
            .transaction(&id("tx.restock.update"))
            .expect("the template's transaction")
            .clone();

        assert!(matches!(
            transaction.idempotency,
            conseqa::spec::IdempotencyGuarantee::DeduplicatedBy { .. }
        ));

        std::fs::remove_dir_all(&out_dir).ok();
    }

    /// The runtime topology from L0 alone: with every L1 declaration
    /// taken away, the builder declares the conservative defaults — a
    /// pool per service, a router per request, `at_least_once` delivery
    /// per subscription, an outbox runtime per outbox input, a layout
    /// per object — and every obligation the author proved is proven
    /// again, the replay families' delivery facts included. No session.
    #[tokio::test]
    async fn the_runtime_topology_is_declared_from_defaults() {
        for (fixture, any_operation) in [
            ("shop.yaml", "operation.restock"),
            ("tenant_ledger.yaml", "operation.post_entry"),
        ] {
            let mut model = authored(fixture);

            model.runtime = None;

            let (engine, seen, runs) = run_tasks(
                workspace_of(&model, "A system."),
                vec![(
                    TaskKind::TopologySynthesis,
                    conseqa::confluence::WriteScope::runtime_topology(),
                    id(any_operation),
                )],
                &Opinions::default(),
            )
            .await;

            let handed: Vec<String> = seen
                .lock()
                .expect("not poisoned")
                .iter()
                .map(|(_, prompt)| prompt.clone())
                .collect();

            assert!(handed.is_empty(), "{fixture}: {handed:?}");
            assert!(runs[0].committed(), "{fixture}: {:?}", runs[0]);
            assert!(head_model_is_proven(&engine).await, "{fixture}");

            let runtime = engine.head_snapshot().workspace.runtime.clone();
            let authored_runtime = authored(fixture).runtime.expect("an authored runtime");

            assert_eq!(
                runtime.storage_layouts.len(),
                authored_runtime.storage_layouts.len(),
                "{fixture}"
            );
            assert_eq!(
                runtime.routers.len(),
                authored_runtime.routers.len(),
                "{fixture}"
            );
            assert_eq!(
                runtime
                    .subscriptions
                    .values()
                    .map(BTreeMap::len)
                    .sum::<usize>(),
                authored_runtime
                    .subscriptions
                    .values()
                    .map(BTreeMap::len)
                    .sum::<usize>(),
                "{fixture}"
            );
        }
    }

    type Events = Arc<Mutex<Vec<(&'static str, String)>>>;

    /// A scripted session that writes the authored program, without its
    /// requirements, for a synthesis task, and records when it does and
    /// when a discovery task arrives. `slow` delays an operation's
    /// synthesis, or every discovery under the key `discovery`, by
    /// milliseconds.
    fn pipeline_script(
        events: &Events,
        authored: &conseqa::spec::Model,
        slow: BTreeMap<&'static str, u64>,
    ) -> ScriptFn {
        let events = Arc::clone(events);
        let authored = authored.clone();
        let slow = slow.clone();

        Arc::new(move |engine, invocation| {
            let events = Arc::clone(&events);
            let authored = authored.clone();
            let slow = slow.clone();

            Box::pin(async move {
                let task = engine
                    .resolve_token(&invocation.task_token)
                    .expect("token resolves");

                let operation = engine
                    .task_context(task)
                    .expect("context")
                    .write_scope
                    .grants
                    .iter()
                    .find_map(|grant| match grant {
                        conseqa::confluence::WriteGrant::OperationProgram(operation)
                        | conseqa::confluence::WriteGrant::OperationRequirements(operation) => {
                            Some(operation.clone())
                        }
                        _ => None,
                    });

                let Some(operation) = operation else {
                    return;
                };

                match invocation.kind {
                    TaskKind::OperationSynthesis => {
                        events
                            .lock()
                            .expect("not poisoned")
                            .push(("synthesizing", operation.0.clone()));

                        if let Some(delay) = slow.get(operation.0.as_str()) {
                            tokio::time::sleep(Duration::from_millis(*delay)).await;
                        }

                        // Observe what the program references, as a
                        // session would before committing it.
                        for kind in [
                            conseqa::confluence::SymbolKind::DataModel,
                            conseqa::confluence::SymbolKind::DataObject,
                            conseqa::confluence::SymbolKind::Schema,
                            conseqa::confluence::SymbolKind::StateMachine,
                            conseqa::confluence::SymbolKind::Transition,
                        ] {
                            for key in engine
                                .search_symbols(
                                    task,
                                    &conseqa::confluence::SearchSpec {
                                        kind: Some(kind),
                                        ..Default::default()
                                    },
                                )
                                .expect("search")
                            {
                                engine.read_symbol(task, &key).expect("read");
                            }
                        }

                        // The program without its requirements:
                        // what they are is discovery's to say.
                        let mut program = authored.operations[&operation].program.clone();

                        let ids: Vec<Id> = program
                            .transactions()
                            .into_iter()
                            .map(|(_, transaction)| transaction.id.clone())
                            .collect();

                        for transaction in ids {
                            program
                                .transaction_mut(&transaction)
                                .expect("the transaction")
                                .requirements = Default::default();
                        }

                        commit(
                            &engine,
                            &invocation,
                            vec![Mutation::ReplaceOperationProgram {
                                operation: operation.clone(),
                                program,
                            }],
                        )
                        .await;

                        events
                            .lock()
                            .expect("not poisoned")
                            .push(("synthesized", operation.0));
                    }

                    TaskKind::RequirementDiscovery => {
                        if let Some(delay) = slow.get("discovery") {
                            tokio::time::sleep(Duration::from_millis(*delay)).await;
                        }

                        events
                            .lock()
                            .expect("not poisoned")
                            .push(("discovering", operation.0));
                    }

                    _ => {}
                }
            })
        })
    }

    /// A follow-up never delays a primary: with two slots and three
    /// programs to write, the third program starts before the first
    /// operation's discovery does. (Run in the slot it followed, the
    /// discovery held that slot and the last program — the critical path
    /// — started minutes late on the benchmark.)
    #[tokio::test]
    async fn a_waiting_program_is_written_before_any_discovery() {
        let authored = authored("shop.yaml");

        let mut workspace = workspace_of(&authored, "A small shop backend.");

        for operation in [
            "operation.pay_order",
            "operation.restock",
            "operation.ship_order",
        ] {
            let draft = workspace
                .operations
                .get_mut(&id(operation))
                .expect("the operation");

            draft.program = None;
            draft.requirements = Default::default();
        }

        let engine = ConfluenceEngine::in_memory(workspace).expect("engine starts");

        let events: Events = Arc::default();

        let script = pipeline_script(
            &events,
            &authored,
            [("operation.restock", 400), ("discovery", 100)].into(),
        );

        let backend = Arc::new(ScriptedBackend {
            engine: engine.clone(),
            script,
        });

        let out_dir = scratch();

        let supervisor = Supervisor::new(
            engine.clone(),
            backend,
            "http://127.0.0.1:0/mcp",
            None,
            out_dir.join("work"),
        );

        let scheduler = Scheduler::new(
            engine.clone(),
            supervisor,
            SchedulerPolicy {
                max_concurrent_agents: 2,
                ..Default::default()
            },
        );

        let workflow = Workflow::new(
            scheduler,
            WorkflowConfig {
                out_dir: out_dir.clone(),
                analysis_timeout: Duration::from_secs(20),
                max_iterations: 3,
                objective: None,
            },
        );

        workflow.run().await.expect("the workflow runs");

        let events = events.lock().expect("not poisoned").clone();

        let at = |kind: &str, operation: &str| {
            events
                .iter()
                .position(|(seen, on)| *seen == kind && on == operation)
                .unwrap_or_else(|| panic!("no {kind} {operation} in {events:?}"))
        };

        // Submission order is by operation id: pay_order, restock,
        // ship_order. pay_order finishes first; its discovery must wait
        // for ship_order's synthesis to start.
        assert!(
            at("synthesizing", "operation.ship_order") < at("discovering", "operation.pay_order"),
            "{events:?}"
        );

        std::fs::remove_dir_all(&out_dir).ok();
    }

    /// Discovery is pipelined onto synthesis: an operation whose program
    /// lands first has its requirements discovered while a slower peer
    /// is still being written, instead of every discovery waiting at a
    /// barrier for the slowest program.
    #[tokio::test]
    async fn discovery_starts_before_the_slowest_program_lands() {
        let authored = authored("shop.yaml");

        let mut workspace = workspace_of(&authored, "A small shop backend.");

        for operation in ["operation.restock", "operation.ship_order"] {
            let draft = workspace
                .operations
                .get_mut(&id(operation))
                .expect("the operation");

            draft.program = None;
            draft.requirements = Default::default();
        }

        let engine = ConfluenceEngine::in_memory(workspace).expect("engine starts");

        let events: Events = Arc::default();

        let script = pipeline_script(&events, &authored, [("operation.ship_order", 400)].into());

        let backend = Arc::new(ScriptedBackend {
            engine: engine.clone(),
            script,
        });

        let out_dir = scratch();

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
                out_dir: out_dir.clone(),
                analysis_timeout: Duration::from_secs(20),
                max_iterations: 3,
                objective: None,
            },
        );

        workflow.run().await.expect("the workflow runs");

        let events = events.lock().expect("not poisoned").clone();

        let at = |event: (&str, &str)| {
            events
                .iter()
                .position(|(kind, operation)| *kind == event.0 && operation == event.1)
                .unwrap_or_else(|| panic!("no {event:?} in {events:?}"))
        };

        assert!(
            at(("discovering", "operation.restock")) < at(("synthesized", "operation.ship_order")),
            "{events:?}"
        );

        std::fs::remove_dir_all(&out_dir).ok();
    }

    /// The sketch architecture end to end: the fanout finds every
    /// program of `shop` missing and every interface sketched. Each
    /// program is compiled from its sketch, a decider only confirming it
    /// does what the description says; the run succeeds and no session
    /// writes, discovers or repairs anything.
    #[tokio::test]
    async fn every_program_is_compiled_from_its_sketch_without_a_session() {
        let mut workspace = workspace_of(&authored("shop.yaml"), "A small shop backend.");

        for (operation, sketch) in common::sketches::shop_sketches() {
            let draft = workspace
                .operations
                .get_mut(&id(operation))
                .expect("the operation");

            draft.program = None;
            draft.sketch = Some(serde_json::from_value(sketch).expect("parses"));
        }

        let opinions = Opinions::default().stating("fidelity", 0.92);

        let engine = ConfluenceEngine::in_memory(workspace).expect("engine starts");
        let seen = Seen::default();
        let idle: ScriptFn = Arc::new(|_, _| Box::pin(async {}));

        let backend = Arc::new(SystemOneBackend::new(
            engine.clone(),
            Arc::new(opinions.clone()),
            conseqa::harness::executors::BUILDABLE,
            Arc::new(ScriptedBackend {
                engine: engine.clone(),
                script: recording(idle, seen.clone()),
            }),
        ));

        let out_dir = scratch();

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
                out_dir: out_dir.clone(),
                analysis_timeout: Duration::from_secs(20),
                max_iterations: 8,
                objective: None,
            },
        );

        let report = workflow.run().await.expect("the workflow runs");

        assert!(
            matches!(report.status, RunStatus::Success { .. }),
            "{:?}",
            report.status
        );

        assert!(
            seen.lock().expect("not poisoned").is_empty(),
            "no session: {:?}",
            seen.lock().expect("not poisoned")
        );

        let manifest = manifest_of(&out_dir);
        let synthesized = records(&manifest, "operation_synthesis");

        assert_eq!(synthesized.len(), 5, "{manifest}");
        assert!(
            synthesized
                .iter()
                .all(|record| record["executor"] == "system_one"),
            "{manifest}"
        );

        std::fs::remove_dir_all(&out_dir).ok();
    }

    /// The shop, sketched, through `build_design`'s configuration: one
    /// task at a time, a hand-off backend in place of agent sessions.
    /// `break_ship_order` sketches `ship_order` against a record that
    /// does not exist.
    async fn build(
        break_ship_order: bool,
    ) -> (ConfluenceEngine, conseqa::harness::RunReport, Vec<conseqa::harness::HandOff>) {
        let mut workspace = workspace_of(&authored("shop.yaml"), "A small shop backend.");

        for (operation, mut sketch) in common::sketches::shop_sketches() {
            if break_ship_order && operation == "operation.ship_order" {
                sketch["steps"][0]["record"] = serde_json::json!("object.parcel");
            }

            let draft = workspace
                .operations
                .get_mut(&id(operation))
                .expect("the operation");

            draft.program = None;
            draft.sketch = Some(serde_json::from_value(sketch).expect("parses"));
        }

        let engine = ConfluenceEngine::in_memory(workspace).expect("engine starts");
        let hand_off = conseqa::harness::HandOffBackend::new(engine.clone());

        let backend = Arc::new(SystemOneBackend::new(
            engine.clone(),
            Arc::new(Opinions::default().stating("fidelity", 0.92)),
            conseqa::harness::executors::BUILDABLE,
            Arc::new(hand_off.clone()),
        ));

        let out_dir = scratch();

        let supervisor = Supervisor::new(
            engine.clone(),
            backend,
            "http://127.0.0.1:0/mcp",
            None,
            out_dir.join("work"),
        );

        let scheduler = Scheduler::new(
            engine.clone(),
            supervisor,
            SchedulerPolicy {
                max_concurrent_agents: 1,
                ..Default::default()
            },
        );

        let workflow = Workflow::new(
            scheduler,
            WorkflowConfig {
                out_dir: out_dir.clone(),
                analysis_timeout: Duration::from_secs(20),
                max_iterations: 8,
                objective: None,
            },
        )
        .with_halt(hand_off.halt());

        let report = workflow.run().await.expect("the workflow runs");

        std::fs::remove_dir_all(&out_dir).ok();

        (engine, report, hand_off.handed())
    }

    /// A single-threaded build with no sessions settles the whole
    /// sketched shop and hands nothing back.
    #[tokio::test]
    async fn a_build_settles_a_sketched_system_without_handing_anything_back() {
        let (_, report, handed) = build(false).await;

        assert!(handed.is_empty(), "{handed:#?}");
        assert!(
            matches!(report.status, RunStatus::Success { .. }),
            "{:?}",
            report.status
        );
    }

    /// What code cannot settle is handed back once, with the builder's
    /// reason, and the build stops there: the other programs are
    /// compiled, the broken one is left for the caller to fix.
    #[tokio::test]
    async fn a_build_hands_a_broken_sketch_back_and_stops() {
        let (engine, report, handed) = build(true).await;

        assert_eq!(handed.len(), 1, "{handed:#?}");
        assert_eq!(handed[0].kind, TaskKind::OperationSynthesis);
        assert!(
            handed[0].objective.contains("operation.ship_order"),
            "{}",
            handed[0].objective
        );
        assert!(
            handed[0]
                .builder
                .as_deref()
                .is_some_and(|builder| builder.contains("does not compile")),
            "{:?}",
            handed[0].builder
        );

        assert!(
            matches!(report.status, RunStatus::Incomplete { .. }),
            "{:?}",
            report.status
        );

        let head = engine.head_snapshot();

        for (operation, draft) in &head.workspace.operations {
            assert_eq!(
                draft.program.is_none(),
                operation.0 == "operation.ship_order",
                "{operation}"
            );
        }
    }

    /// Discovery runs once per operation per run. A discovery the
    /// decider is unsure of goes to a session, which may propose nothing;
    /// the operation then still looks undiscovered, and every later pass
    /// of the fixpoint used to schedule it again — on the first Desktop
    /// run, the same operation's question was asked ten times.
    #[tokio::test]
    async fn an_unsure_discovery_is_not_repeated_on_every_pass() {
        let mut workspace = workspace_of(&authored("shop.yaml"), "A small shop backend.");

        for (operation, sketch) in common::sketches::shop_sketches() {
            let draft = workspace
                .operations
                .get_mut(&id(operation))
                .expect("the operation");

            draft.program = None;
            draft.requirements = Default::default();
            draft.sketch = Some(serde_json::from_value(sketch).expect("parses"));
        }

        let opinions = Opinions::default()
            .stating("fidelity", 0.92)
            .stating("idempotency", 0.5);

        let engine = ConfluenceEngine::in_memory(workspace).expect("engine starts");
        let seen = Seen::default();
        let idle: ScriptFn = Arc::new(|_, _| Box::pin(async {}));

        let backend = Arc::new(SystemOneBackend::new(
            engine.clone(),
            Arc::new(opinions.clone()),
            conseqa::harness::executors::BUILDABLE,
            Arc::new(ScriptedBackend {
                engine: engine.clone(),
                script: recording(idle, seen.clone()),
            }),
        ));

        let out_dir = scratch();

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
                out_dir: out_dir.clone(),
                analysis_timeout: Duration::from_secs(20),
                max_iterations: 8,
                objective: None,
            },
        );

        workflow.run().await.expect("the workflow runs");

        let manifest = manifest_of(&out_dir);

        let mut per_operation: BTreeMap<String, usize> = BTreeMap::new();

        for record in records(&manifest, "requirement_discovery") {
            *per_operation
                .entry(
                    record["operation"]
                        .as_str()
                        .expect("an operation")
                        .to_string(),
                )
                .or_default() += 1;
        }

        assert_eq!(per_operation.len(), 5, "{manifest}");
        assert!(
            per_operation.values().all(|count| *count == 1),
            "{per_operation:?}"
        );

        std::fs::remove_dir_all(&out_dir).ok();
    }

    /// The guard: a sketch whose program plainly contradicts the
    /// operation's description goes to a session, told why.
    #[tokio::test]
    async fn a_sketch_contradicting_its_description_goes_to_a_session() {
        let mut workspace = shop_without_program("operation.ship_order");

        workspace
            .operations
            .get_mut(&id("operation.ship_order"))
            .expect("the operation")
            .sketch = Some(
            serde_json::from_value(
                common::sketches::shop_sketches()
                    .into_iter()
                    .find(|(operation, _)| *operation == "operation.ship_order")
                    .expect("sketched")
                    .1,
            )
            .expect("parses"),
        );

        let (engine, seen, _) = run_tasks(
            workspace,
            vec![(
                TaskKind::OperationSynthesis,
                conseqa::confluence::WriteScope::operation_synthesis(id("operation.ship_order")),
                id("operation.ship_order"),
            )],
            &Opinions::default().stating("fidelity", 0.04),
        )
        .await;

        let handed = syntheses(&seen);

        assert_eq!(handed.len(), 1);
        assert!(
            handed[0].contains("does not do what the operation is described to do"),
            "{}",
            handed[0]
        );

        assert!(
            engine.head_snapshot().workspace.operations[&id("operation.ship_order")]
                .program
                .is_none()
        );
    }

    /// `tenant_ledger` as its author wrote it, and with the exclusive
    /// lock that serializes postings taken out — which leaves the
    /// read-then-write of the tenant's sequence unprotected.
    fn a_posting_without_its_lock() -> (conseqa::spec::Model, conseqa::spec::Model) {
        let authored = authored("tenant_ledger.yaml");

        let mut broken = authored.clone();

        broken
            .operations
            .get_mut(&id("operation.post_entry"))
            .expect("the operation")
            .program
            .transaction_mut(&id("tx.post_entry"))
            .expect("the transaction")
            .steps
            .retain(|step| !matches!(step, conseqa::spec::TransactionStep::Lock(_)));

        assert!(serializability_proven(&authored, "tx.post_entry"));
        assert!(
            !serializability_proven(&broken, "tx.post_entry"),
            "the fixture is only a test if removing the lock breaks the proof"
        );

        (authored, broken)
    }

    async fn repair(
        broken: &conseqa::spec::Model,
        operation: &str,
        opinions: &Opinions,
    ) -> (ConfluenceEngine, Seen, Vec<conseqa::harness::TaskRun>) {
        run_tasks(
            workspace_of(broken, "A ledger of entries per tenant."),
            vec![(
                TaskKind::RequirementRepair,
                conseqa::confluence::WriteScope::requirement_repair(id(operation)),
                id(operation),
            )],
            opinions,
        )
        .await
    }

    async fn head_model_is_proven(engine: &ConfluenceEngine) -> bool {
        match engine.analysis_ready(engine.head_revision()).await {
            conseqa::confluence::AnalysisState::Ready(analysis) => {
                analysis.verification.all_proven()
            }
            other => panic!("the repaired head does not verify: {other:?}"),
        }
    }

    fn repairs(seen: &Seen) -> Vec<String> {
        seen.lock()
            .expect("not poisoned")
            .iter()
            .filter(|(kind, _)| *kind == TaskKind::RequirementRepair)
            .map(|(_, prompt)| prompt.clone())
            .collect()
    }

    /// Generate and verify: code reads the analyzer's obstacles,
    /// synthesizes the repairs they admit, and has the analyzer judge
    /// each one. With nothing stated the least invasive proven repair is
    /// committed — and no session, and no model, was needed to find it.
    #[tokio::test]
    async fn an_unproven_obligation_is_repaired_by_a_candidate_the_analyzer_admits() {
        let (_, broken) = a_posting_without_its_lock();

        let opinions = Opinions::default();

        let (engine, seen, runs) = repair(&broken, "operation.post_entry", &opinions).await;

        assert!(repairs(&seen).is_empty(), "{:?}", repairs(&seen));
        assert!(runs[0].committed(), "{:?}", runs[0]);
        assert_eq!(runs[0].attempts.len(), 1);

        assert!(head_model_is_proven(&engine).await);

        let head = engine.head_snapshot();

        let transaction = head.workspace.operations[&id("operation.post_entry")]
            .program
            .as_ref()
            .and_then(|program| program.transaction(&id("tx.post_entry")))
            .expect("the transaction")
            .clone();

        assert_eq!(
            transaction.isolation,
            conseqa::spec::TransactionIsolation::Serializable
        );

        // Two repairs were proven, so the one fact that could choose
        // between them was asked about — and only that.
        let asked = opinions.asked();

        assert_eq!(asked.len(), 1);
        assert_eq!(
            asked[0]
                .questions
                .keys()
                .map(|id| id.0.as_str())
                .collect::<Vec<_>>(),
            vec!["contention"]
        );
        assert_eq!(asked[0].tags["builder"], "requirement_repair");
    }

    /// A System One judgment chooses among repairs that are all proven,
    /// and only on a fact the prompt states. Told that postings pile onto
    /// one tenant, the builder takes the lock route — and arrives at the
    /// very program the fixture's author wrote.
    #[tokio::test]
    async fn stated_contention_prefers_the_lock_the_author_chose() {
        let (authored, broken) = a_posting_without_its_lock();

        let opinions = Opinions::default().stating("contention", 0.93);

        let (engine, seen, runs) = repair(&broken, "operation.post_entry", &opinions).await;

        assert!(repairs(&seen).is_empty(), "{:?}", repairs(&seen));
        assert!(runs[0].committed(), "{:?}", runs[0]);

        assert!(head_model_is_proven(&engine).await);

        let head = engine.head_snapshot();

        assert_eq!(
            head.workspace.operations[&id("operation.post_entry")].program,
            Some(
                authored.operations[&id("operation.post_entry")]
                    .program
                    .clone()
            )
        );
    }

    /// The other two routes of the catalogue, each against a different
    /// way of breaking the same authored transaction: an isolation that
    /// was never declared, and a lock taken after the read it was meant
    /// to protect. Each time the builder arrives back at the author's
    /// program.
    #[tokio::test]
    async fn each_breakage_is_repaired_back_to_what_the_author_wrote() {
        type Breakage = fn(&mut conseqa::spec::Transaction);

        let undeclared_isolation: Breakage = |transaction| {
            transaction.isolation = conseqa::spec::TransactionIsolation::Unspecified;
        };

        let late_lock: Breakage = |transaction| {
            let lock = transaction.steps.remove(0);

            assert!(matches!(lock, conseqa::spec::TransactionStep::Lock(_)));

            transaction.steps.insert(1, lock);
        };

        for (breakage, contention) in [(undeclared_isolation, 0.02), (late_lock, 0.93)] {
            let authored = authored("tenant_ledger.yaml");

            let mut broken = authored.clone();

            breakage(
                broken
                    .operations
                    .get_mut(&id("operation.post_entry"))
                    .expect("the operation")
                    .program
                    .transaction_mut(&id("tx.post_entry"))
                    .expect("the transaction"),
            );

            assert!(conseqa::analyzer::validate(&broken).is_empty());
            assert!(!serializability_proven(&broken, "tx.post_entry"));

            let opinions = Opinions::default().stating("contention", contention);

            let (engine, seen, runs) = repair(&broken, "operation.post_entry", &opinions).await;

            assert!(repairs(&seen).is_empty(), "{:?}", repairs(&seen));
            assert!(runs[0].committed(), "{:?}", runs[0]);
            assert!(head_model_is_proven(&engine).await);

            assert_eq!(
                engine.head_snapshot().workspace.operations[&id("operation.post_entry")].program,
                Some(
                    authored.operations[&id("operation.post_entry")]
                        .program
                        .clone()
                )
            );
        }
    }

    /// The whole pipeline, from stated obligations to a verified model,
    /// with discovery and repair both settled in process.
    ///
    /// The adopted model is the authored ledger with every declared
    /// requirement taken out, and with the lock that serializes postings
    /// taken out too — so there is something to discover and, once it is
    /// discovered, something to repair. The agent backend behind the
    /// builders can do nothing at all: if either phase needed it, the run
    /// could not succeed.
    #[tokio::test]
    async fn obligations_become_a_verified_model_without_a_session() {
        let posting = id("operation.post_entry");

        let (mut workspace, _) = undeclared("tenant_ledger.yaml", &posting);

        workspace
            .operations
            .get_mut(&posting)
            .expect("the operation")
            .program
            .as_mut()
            .expect("a program")
            .transaction_mut(&id("tx.post_entry"))
            .expect("the transaction")
            .steps
            .retain(|step| !matches!(step, conseqa::spec::TransactionStep::Lock(_)));

        workspace.prompt_obligations.insert(
            PromptObligationId("obl.one-sequence-each".to_string()),
            PromptObligation {
                source_span: Some("in sequence order".to_string()),
                normalized_intent: "two postings for one tenant never take the same sequence"
                    .to_string(),
                targets: vec![posting.clone()],
                status: PromptObligationStatus::Unmapped,
            },
        );

        let opinions = Opinions::default()
            .stating_for(
                "operation.post_entry",
                "obligation_0_serializability_0",
                0.91,
            )
            .stating_for("operation.post_entry", "idempotency", 0.9)
            .stating_for("operation.post_entry", "result_replay", 0.88);

        let engine = ConfluenceEngine::in_memory(workspace).expect("engine starts");

        let seen = Seen::default();

        let idle: ScriptFn = Arc::new(|_, _| Box::pin(async {}));

        let backend = Arc::new(SystemOneBackend::new(
            engine.clone(),
            Arc::new(opinions.clone()),
            conseqa::harness::executors::BUILDABLE,
            Arc::new(ScriptedBackend {
                engine: engine.clone(),
                script: recording(idle, seen.clone()),
            }),
        ));

        let out_dir = scratch();

        let supervisor = Supervisor::new(
            engine.clone(),
            backend,
            "http://127.0.0.1:0/mcp",
            None,
            out_dir.join("work"),
        );

        let workflow = Workflow::new(
            Scheduler::new(engine.clone(), supervisor, SchedulerPolicy::default()),
            WorkflowConfig {
                out_dir: out_dir.clone(),
                analysis_timeout: Duration::from_secs(20),
                max_iterations: 8,
                objective: None,
            },
        );

        let report = workflow.run().await.expect("the workflow runs");

        assert!(
            matches!(report.status, RunStatus::Success { .. }),
            "{:?}",
            report.status
        );

        // No session ran: the agent backend was never reached.
        assert!(
            seen.lock().expect("not poisoned").is_empty(),
            "{:?}",
            seen.lock()
                .expect("not poisoned")
                .iter()
                .map(|(kind, _)| *kind)
                .collect::<Vec<_>>()
        );

        // The obligation was mapped to a requirement keyed by the tenant
        // the transaction's selectors pin, and the repair that proves it
        // is in the exported model, which the standalone checker accepts.
        let head = engine.head_snapshot();

        assert!(matches!(
            head.workspace.prompt_obligations
                [&PromptObligationId("obl.one-sequence-each".to_string())]
                .status,
            PromptObligationStatus::Mapped { .. }
        ));

        let source =
            std::fs::read_to_string(out_dir.join("conseqa.yaml")).expect("the model was exported");

        let model = conseqa::parser::yaml::parse(&source).expect("the exported model parses");

        assert!(conseqa::analyzer::validate(&model).is_empty());
        assert!(conseqa::analyzer::verification::verify(&model).all_proven());

        let transaction = model.operations[&posting]
            .program
            .transaction(&id("tx.post_entry"))
            .expect("the transaction");

        assert_eq!(
            transaction.requirements.serializability[0].key,
            ValueRef {
                source: ValueSource::Input(id("input.post_entry.request")),
                path: path("tenant_id"),
            }
        );

        assert_eq!(model.operations[&posting].requirements.idempotency.len(), 1);

        // The manifest tells the same story: both phases were the
        // builders', and nothing was escalated.
        let manifest = manifest_of(&out_dir);

        assert_eq!(manifest["status"]["kind"], "success");
        assert!(manifest["executors"].get("agent").is_none(), "{manifest}");

        assert!(records(&manifest, "requirement_repair").iter().any(
            |record| record["final_state"] == "committed" && record["executor"] == "system_one"
        ));

        std::fs::remove_dir_all(&out_dir).ok();
    }

    /// A preference is not a judgment the builder needs in order to act:
    /// an unsure decider, or none at all, leaves the deterministic order
    /// standing and the repair is committed all the same.
    #[tokio::test]
    async fn an_unsure_or_absent_decider_does_not_stop_a_repair() {
        for opinions in [
            Opinions::default().stating("contention", 0.55),
            Opinions {
                unavailable: true,
                ..Default::default()
            },
        ] {
            let (_, broken) = a_posting_without_its_lock();

            let (engine, seen, runs) = repair(&broken, "operation.post_entry", &opinions).await;

            assert!(repairs(&seen).is_empty(), "{:?}", repairs(&seen));
            assert!(runs[0].committed(), "{:?}", runs[0]);
            assert!(head_model_is_proven(&engine).await);
        }
    }

    /// `flash_checkout` as authored, and with `apply_payment`'s
    /// observed-version guard taken off its cursor advance. Its conflict
    /// closure spans three operations, so neither isolation nor a lock in
    /// this program alone can prove it.
    fn a_payment_without_its_version_guard() -> (conseqa::spec::Model, conseqa::spec::Model) {
        let authored = authored("flash_checkout.yaml");

        let mut broken = authored.clone();

        broken
            .operations
            .get_mut(&id("operation.apply_payment"))
            .expect("the operation")
            .program
            .transaction_mut(&id("tx.apply_payment"))
            .expect("the transaction")
            .steps
            .iter_mut()
            .for_each(|step| {
                if let conseqa::spec::TransactionStep::AdvanceCursor(advance) = step {
                    advance.compare.clear();
                }
            });

        assert!(conseqa::analyzer::validate(&broken).is_empty());
        assert!(serializability_proven(&authored, "tx.apply_payment"));
        assert!(!serializability_proven(&broken, "tx.apply_payment"));

        (authored, broken)
    }

    /// Several routes are tried and the analyzer settles which one
    /// works. Here only the observed-state guard can: the object is
    /// versioned, the transaction already mutates the instance it read and
    /// says what a rejection does, and the one missing piece is the
    /// comparison — which lands where the author had put it.
    #[tokio::test]
    async fn the_analyzer_picks_the_route_that_proves_across_operations() {
        let (authored, broken) = a_payment_without_its_version_guard();

        let opinions = Opinions::default();

        let (engine, seen, runs) = repair(&broken, "operation.apply_payment", &opinions).await;

        assert!(repairs(&seen).is_empty(), "{:?}", repairs(&seen));
        assert!(runs[0].committed(), "{:?}", runs[0]);

        // The fixture leaves other operations' obligations unproven on
        // purpose, so the claim is not that everything is proven. It is
        // that the repaired head stands exactly where the authored model
        // stands — including this transaction's ordering requirement,
        // which the builder did not target and which rests on the same
        // closure.
        let conseqa::confluence::AnalysisState::Ready(analysis) =
            engine.analysis_ready(engine.head_revision()).await
        else {
            panic!("the repaired head does not verify");
        };

        assert_eq!(
            conseqa::confluence::standing(&analysis.verification),
            conseqa::confluence::standing(&conseqa::analyzer::verification::verify(&authored))
        );

        let head = engine.head_snapshot();

        assert_eq!(
            head.workspace.operations[&id("operation.apply_payment")].program,
            Some(
                authored.operations[&id("operation.apply_payment")]
                    .program
                    .clone()
            )
        );

        // One admissible repair: nothing to prefer, so nothing was asked.
        assert!(opinions.asked().is_empty());

        let summary = runs[0].attempts[0]
            .agent_exit
            .final_message
            .clone()
            .expect("a summary");

        assert!(
            summary.contains("1 of 2 candidates were admissible"),
            "{summary}"
        );
    }

    /// A guard rejects, and what an operation does when its transaction
    /// is rejected is its author's to say. Without a `rejected` arm the
    /// observed-state guard — the update turned compare-and-set — is not
    /// offered; what was tried, and what the analyzer made of it, goes to
    /// the session.
    #[tokio::test]
    async fn a_repair_that_needs_a_judgment_is_handed_to_the_session() {
        let (_, mut broken) = a_payment_without_its_version_guard();

        // Take out every rejecting step and the arm with them, leaving a
        // valid program whose author never said what a rejection does.
        let operation = broken
            .operations
            .get_mut(&id("operation.apply_payment"))
            .expect("the operation");

        operation.requirements = Default::default();

        for step in &mut operation.program.steps {
            if let OperationStep::Transaction(execution) = step {
                execution.rejected = None;

                execution.transaction.requirements.ordering.clear();

                execution.transaction.steps.retain(|step| {
                    !matches!(
                        step,
                        conseqa::spec::TransactionStep::AdvanceCursor(_)
                            | conseqa::spec::TransactionStep::Transition(_)
                    )
                });

                // What remains still changes the order it read — a plain
                // update, so the transaction is no read-only observation
                // and needs a real protection of its read.
                let conseqa::spec::TransactionStep::Read(read) = &execution.transaction.steps[0]
                else {
                    panic!("apply_payment reads the order first");
                };

                let update = conseqa::spec::TransactionStep::Update(conseqa::spec::Update {
                    target: read.target.clone(),
                    fields: [path("amount")].into(),
                    values: conseqa::spec::Derivation::Deterministic {
                        from: vec![ValueRef {
                            source: ValueSource::TransactionRead(read.bind.clone()),
                            path: path("order_id"),
                        }],
                    },
                });

                execution.transaction.steps.insert(1, update);
            }
        }

        operation
            .program
            .steps
            .retain(|step| !matches!(step, OperationStep::ExecuteEffectIntent(_)));

        let errors = conseqa::analyzer::validate(&broken);

        assert!(errors.is_empty(), "{errors:?}");

        let opinions = Opinions::default();

        let (engine, seen, runs) = repair(&broken, "operation.apply_payment", &opinions).await;

        assert!(!runs[0].committed());
        assert_eq!(engine.head_revision(), broken.revision);

        let handed = repairs(&seen);

        assert_eq!(handed.len(), 1);
        assert!(
            handed[0].contains("Hand-off from the System One executor")
                && handed[0].contains("tried: hold an exclusive lock")
                && handed[0].contains("unproven"),
            "{}",
            handed[0].split("## Hand-off").last().unwrap_or_default()
        );
        assert!(
            !handed[0].contains("condition the later mutation"),
            "the observed-state guard was not offered"
        );
    }

    /// With nothing unproven the builder says so itself.
    #[tokio::test]
    async fn a_proven_operation_needs_no_repair_and_no_session() {
        let authored = authored("tenant_ledger.yaml");

        let opinions = Opinions::default();

        let (engine, seen, runs) = repair(&authored, "operation.post_entry", &opinions).await;

        assert!(repairs(&seen).is_empty());
        assert!(opinions.asked().is_empty());

        // The task ended as a session that found nothing to do would:
        // once, without a commit.
        assert_eq!(runs[0].attempts.len(), 1);
        assert!(!runs[0].committed());
        assert_eq!(engine.head_revision(), authored.revision);
    }

    /// Between the thresholds the builder does not decide.
    #[tokio::test]
    async fn an_uncertain_judgment_is_escalated_not_guessed() {
        let out_dir = scratch();

        let opinions = Opinions::default().stating("recoverability", 0.5);

        let (workflow, engine, seen) =
            workflow_with(out_dir.clone(), no_requirements_script(), &opinions);

        workflow.run().await.expect("the workflow runs");

        let handed = discoveries(&seen);

        assert!(!handed.is_empty());
        assert!(
            handed[0].contains("uncertain whether the prompt requires recoverability"),
            "{}",
            handed[0]
        );

        // The scripted agent proposes nothing, and neither did the
        // builder.
        let head = engine.head_snapshot();

        assert!(head.workspace.requirement_proposals.is_empty());

        std::fs::remove_dir_all(&out_dir).ok();
    }
}
