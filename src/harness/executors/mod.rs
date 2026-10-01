//! The in-process System One executor (§14–§16 of the System One
//! orchestration revision).
//!
//! Every other backend maps a task to an external agent session and
//! never interprets Conseqa semantics. This one does: for the task
//! kinds it is enabled for, it runs a *builder* in process — code that
//! owns the control flow, enumerates what could be decided from the
//! symbol graph, and asks a System One decider only what code cannot
//! answer.
//!
//! It is bound by every rule that binds an external agent. It acts
//! under the task's token and write scope; every fact that influences
//! a decision is obtained through the task-scoped engine API, so the
//! read-set invariant holds; and it commits only through the gate.
//!
//! A builder either commits or *abstains*. It abstains when the task
//! is outside what it enumerates, when an answer it needs in order to
//! act is uncertain, when the decider is unavailable, or when the gate
//! refuses its patch for a reason other than staleness. The same task
//! — same token, same scope, the read-set accumulated so far — is then
//! run by the fallback agent backend, told what the builder
//! established. Nothing is ever committed before an abstention, so a
//! task has at most one committer, and the worst case of enabling the
//! executor is the baseline plus the builder's milliseconds.

use std::sync::Arc;

use async_trait::async_trait;

use crate::confluence::{AgentBackendMetadata, ConfluenceEngine, TaskId, TaskKind};
use crate::harness::backend::{
    AgentBackend, AgentBackendError, AgentEvent, AgentEventSink, AgentExit, AgentExitStatus,
    AgentHandle, AgentInvocation, AgentUsage, Escalation, SYSTEM_ONE_EXECUTOR as BACKEND_NAME,
};
use crate::system_one::Decider;

pub mod cli;
mod describe;
pub mod discovery;
pub mod remedies;
pub mod repair;
pub mod synthesis;
pub mod topology;

pub use discovery::DiscoveryPolicy;
pub use repair::RepairPolicy;
pub use synthesis::SynthesisPolicy;

/// What a builder did with its task.
#[derive(Debug, Clone, PartialEq)]
pub enum Built {
    /// The builder's patch was committed.
    Committed { summary: String },

    /// There was nothing to commit, and the builder is confident of it.
    /// The task ends as an agent's would had it found nothing to do.
    NothingToDo { summary: String },

    /// The gate found the builder's context stale. The task is
    /// invalidated and its successor re-derives from the new head:
    /// re-derivation is cheaper than reconciliation.
    Stale,

    /// The builder will not decide this task. Nothing was committed.
    Abstained(Abstention),
}

/// Why a builder handed its task to the agent backend, and what it had
/// established by then — so the session does not retry dead ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Abstention {
    pub reason: String,

    /// Facts and rejected attempts, one per line in the hand-off.
    pub findings: Vec<String>,
}

impl Abstention {
    pub fn because(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            findings: Vec::new(),
        }
    }

    pub fn with_findings(mut self, findings: Vec<String>) -> Self {
        self.findings = findings;
        self
    }

    /// The section appended to the agent's prompt.
    fn hand_off(&self) -> String {
        let mut text = format!(
            "\n\n## Hand-off from the System One executor\n\n\
             An in-process builder attempted this task first and abstained without committing \
             anything: {}.\n",
            self.reason
        );

        if !self.findings.is_empty() {
            text.push_str("\nWhat it had established:\n");

            for finding in &self.findings {
                text.push_str("- ");
                text.push_str(finding);
                text.push('\n');
            }
        }

        text
    }
}

/// What a builder needs: the engine, under one task's capability, and
/// a decider.
pub struct BuildContext<'a> {
    pub engine: &'a ConfluenceEngine,
    pub decider: &'a dyn Decider,
    pub task: TaskId,
}

/// The task kinds with a builder.
pub const BUILDABLE: [TaskKind; 4] = [
    TaskKind::OperationSynthesis,
    TaskKind::TopologySynthesis,
    TaskKind::RequirementDiscovery,
    TaskKind::RequirementRepair,
];

/// Thresholds for every builder. Provisional: each is to be chosen
/// from the decision log, per question and per backend (§13.3), and
/// none is carried from one backend to another.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SystemOnePolicy {
    pub discovery: DiscoveryPolicy,
    pub repair: RepairPolicy,
    pub synthesis: SynthesisPolicy,
}

/// An [`AgentBackend`] that runs enabled task kinds in process and
/// everything else — and every abstention — on its fallback.
pub struct SystemOneBackend {
    engine: ConfluenceEngine,
    decider: Arc<dyn Decider>,
    kinds: Vec<TaskKind>,
    policy: SystemOnePolicy,
    fallback: Arc<dyn AgentBackend>,
    name: String,
}

impl SystemOneBackend {
    /// `kinds` are the task kinds attempted in process; a kind with no
    /// builder is ignored.
    pub fn new(
        engine: ConfluenceEngine,
        decider: Arc<dyn Decider>,
        kinds: impl IntoIterator<Item = TaskKind>,
        fallback: Arc<dyn AgentBackend>,
    ) -> Self {
        let name = format!("{BACKEND_NAME}+{}", fallback.name());

        Self {
            engine,
            decider,
            kinds: kinds
                .into_iter()
                .filter(|kind| BUILDABLE.contains(kind))
                .collect(),
            policy: SystemOnePolicy::default(),
            fallback,
            name,
        }
    }

    pub fn with_policy(mut self, policy: SystemOnePolicy) -> Self {
        self.policy = policy;
        self
    }

    /// The kinds attempted in process.
    pub fn kinds(&self) -> &[TaskKind] {
        &self.kinds
    }

    async fn build(&self, kind: TaskKind, context: &BuildContext<'_>) -> Built {
        match kind {
            TaskKind::RequirementDiscovery => {
                discovery::build(context, &self.policy.discovery).await
            }

            TaskKind::RequirementRepair => repair::build(context, &self.policy.repair).await,

            TaskKind::OperationSynthesis => synthesis::build(context, &self.policy.synthesis).await,

            TaskKind::TopologySynthesis => topology::build(context).await,

            _ => Built::Abstained(Abstention::because(format!(
                "no builder handles {kind} tasks"
            ))),
        }
    }
}

fn completed(session: Option<String>, message: String) -> AgentExit {
    AgentExit {
        status: AgentExitStatus::Completed,
        session: session.clone(),
        final_message: Some(message),
        usage: AgentUsage::default(),
        backend: AgentBackendMetadata {
            name: BACKEND_NAME.to_string(),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            session,
        },
        escalation: None,
    }
}

#[async_trait]
impl AgentBackend for SystemOneBackend {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(
        &self,
        invocation: AgentInvocation,
        handle: AgentHandle,
        events: AgentEventSink,
    ) -> Result<AgentExit, AgentBackendError> {
        if !self.kinds.contains(&invocation.kind) {
            return self.fallback.run(invocation, handle, events).await;
        }

        // The capability resolves exactly as a real agent's MCP request
        // would; a token that does not resolve is the fallback's to
        // report.
        let Some(task) = self.engine.resolve_token(&invocation.task_token) else {
            return self.fallback.run(invocation, handle, events).await;
        };

        let session = Some(format!("{BACKEND_NAME}:{task}"));

        let _ = events.send(AgentEvent::SessionStarted {
            session: session.clone(),
        });

        let context = BuildContext {
            engine: &self.engine,
            decider: self.decider.as_ref(),
            task,
        };

        let built = tokio::select! {
            built = self.build(invocation.kind, &context) => built,

            () = handle.cancel.cancelled() => {
                return Ok(AgentExit {
                    status: AgentExitStatus::Cancelled,
                    ..completed(session, "cancelled before the builder finished".to_string())
                });
            }
        };

        let abstention = match built {
            Built::Committed { summary } | Built::NothingToDo { summary } => {
                let _ = events.send(AgentEvent::Log {
                    message: summary.clone(),
                });

                return Ok(completed(session, summary));
            }

            Built::Stale => {
                return Ok(completed(
                    session,
                    "the gate found the builder's context stale".to_string(),
                ));
            }

            Built::Abstained(abstention) => abstention,
        };

        let _ = events.send(AgentEvent::Log {
            message: format!(
                "system one abstained from {task} ({}): {}",
                invocation.kind, abstention.reason
            ),
        });

        let mut invocation = invocation;

        invocation.prompt.push_str(&abstention.hand_off());

        let mut exit = self.fallback.run(invocation, handle, events).await?;

        exit.escalation = Some(Escalation {
            from: BACKEND_NAME.to_string(),
            reason: abstention.reason,
        });

        Ok(exit)
    }
}
