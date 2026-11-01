//! The conformance review workflow (§14.3 of the conformance revision):
//! one fresh-context agent per record, through the backends `conseqa
//! design` uses.
//!
//! A review is an independent read of a record against the code, not
//! the attester's argument repeated back. So each agent starts fresh,
//! with the record, the declaration's work item, and read access to the
//! project's files, but none of the attester's reasoning, and reports
//! through `record_review` and nothing else. Its credential, minted for
//! it, stamps its review with the backend and model that made it and
//! the job it ran in, so a review can claim no other reviewer.
//!
//! The agents reach a review server this workflow starts in process,
//! which reads the spec from the project's files: it takes no writer
//! lock, so reviews run beside an open design server. Their generated
//! configuration lives in a scratch directory outside the repository,
//! removed when the run ends.

use std::fmt::Write as _;
use std::path::Path;

use futures::stream::{self, StreamExt};
use tokio_util::sync::CancellationToken;

use crate::analyzer::basis::TextualKey;
use crate::conformance::document::{ReviewOutcome, ReviewResult};
use crate::conformance::packet;
use crate::conformance::record::{Party, PartyKind, RecordContent};
use crate::conformance::review::ReviewRefusal;
use crate::conformance::state::{ConditionKind, Derivation};
use crate::conformance::{Digest, state};
use crate::design::mcp::{self, CredentialRole, Credentials};
use crate::design::store::ProjectStore;
use crate::design::task::TaskId;

use super::backend::{
    AgentBackendError, AgentEventSink, AgentExit, AgentExitStatus, AgentHandle, InvocationBudget,
    SessionLaunch,
};
use super::backends::{ClaudeCliBackend, CodexCliBackend};

/// The backend a review's agents run on.
#[derive(Debug, Clone)]
pub enum ReviewBackend {
    Claude(ClaudeCliBackend),
    Codex(CodexCliBackend),
}

impl ReviewBackend {
    async fn launch(
        &self,
        launch: SessionLaunch,
        handle: &AgentHandle,
        events: &AgentEventSink,
    ) -> Result<AgentExit, AgentBackendError> {
        match self {
            Self::Claude(backend) => backend.launch(launch, handle, events).await,
            Self::Codex(backend) => backend.launch(launch, handle, events).await,
        }
    }
}

/// Who reviews: a backend, and the name its reviews are stamped with.
#[derive(Debug, Clone)]
pub struct Reviewer {
    pub backend: ReviewBackend,

    /// The provider, and the model when one is pinned: `claude`, or
    /// `claude/claude-opus-5-5`.
    pub name: String,
}

/// What to review, and how.
#[derive(Debug, Clone)]
pub struct ReviewRun {
    /// The declarations whose records to review. None means every
    /// record with an item the policy requires reviewed that no review
    /// confirms.
    pub keys: Vec<TextualKey>,

    /// How many agents run at once.
    pub max_agents: usize,

    pub budget: InvocationBudget,
}

/// One record an agent is given.
struct Target {
    key: TextualKey,
    path: String,
    text: String,
    digest: Digest,
    packet: String,
}

/// Runs the review workflow on the project at `project_dir`: what
/// became of each review asked for, in the order asked. A declaration
/// whose record cannot be reviewed as it stands is reported refused,
/// and given no agent.
pub async fn review(
    project_dir: &Path,
    reviewer: &Reviewer,
    run: &ReviewRun,
) -> Result<Vec<ReviewOutcome>, String> {
    let read = ProjectStore::read(project_dir)
        .map_err(|error| format!("cannot read {}: {error}", project_dir.display()))?;

    let derivation =
        Derivation::of(project_dir, &read.workspace).map_err(|error| error.to_string())?;

    let keys: Vec<TextualKey> = if run.keys.is_empty() {
        derivation
            .state
            .declarations
            .iter()
            .filter(|state| state.has(ConditionKind::Unreviewed))
            .map(|state| state.declaration.clone())
            .collect()
    } else {
        run.keys.clone()
    };

    let mut asked: Vec<Result<Target, ReviewOutcome>> = Vec::new();

    for key in keys {
        asked.push(target(&derivation, &key).map_err(|refusal| ReviewOutcome {
            declaration: key,
            by: None,
            result: ReviewResult::of(Err(refusal)),
        }));
    }

    if !asked.iter().any(Result::is_ok) {
        return Ok(asked.into_iter().filter_map(Result::err).collect());
    }

    let credentials = Credentials::default();

    let server = mcp::serve_review(
        project_dir.to_path_buf(),
        credentials.clone(),
        "127.0.0.1:0".parse().expect("a loopback address"),
    )
    .await
    .map_err(|error| format!("cannot start the review server: {error}"))?;

    let url = format!("http://{}/mcp", server.local_addr);

    // Outside the repository, so nothing the agents' tools generate can
    // be committed, or scanned.
    let run_id = uuid::Uuid::new_v4().simple().to_string();
    let scratch = std::env::temp_dir().join(format!("conseqa-review-{run_id}"));

    let outcomes: Vec<ReviewOutcome> = stream::iter(asked.into_iter().enumerate())
        .map(|(index, asked)| {
            let credentials = credentials.clone();
            let url = url.clone();
            let scratch = scratch.clone();
            let job = format!("review-{}-{}", &run_id[..8], index + 1);

            async move {
                match asked {
                    Err(outcome) => outcome,
                    Ok(target) => {
                        launch(
                            project_dir,
                            reviewer,
                            run,
                            &credentials,
                            &url,
                            &scratch,
                            job,
                            target,
                        )
                        .await
                    }
                }
            }
        })
        .buffered(run.max_agents.max(1))
        .collect()
        .await;

    server.shutdown().await;

    let _ = std::fs::remove_dir_all(&scratch);

    Ok(outcomes)
}

/// The record of `key` an agent can review, or why there is none.
fn target(derivation: &Derivation, key: &TextualKey) -> Result<Target, ReviewRefusal> {
    if !derivation.partition.members.contains_key(key.key()) {
        return Err(ReviewRefusal::UnknownDeclaration);
    }

    let Some(file) = derivation.records.get(key) else {
        return Err(ReviewRefusal::NoRecord { set_aside: None });
    };

    if let RecordContent::OtherVersion(mismatch) = &file.content {
        return Err(ReviewRefusal::NoRecord {
            set_aside: Some(mismatch.to_string()),
        });
    }

    let declaration = derivation
        .state
        .declaration(key)
        .expect("a declaration with a record is a bindable declaration of the spec");

    if declaration.has(ConditionKind::Drifted) {
        return Err(ReviewRefusal::Drifted);
    }

    if declaration.has(ConditionKind::Stale) {
        return Err(ReviewRefusal::Stale);
    }

    let text = std::fs::read_to_string(&file.path).map_err(|error| ReviewRefusal::Malformed {
        message: format!("cannot read {}: {error}", file.path.display()),
    })?;

    Ok(Target {
        key: key.clone(),
        path: state::relative(&derivation.project_dir, &file.path),
        text,
        digest: file.digest,
        packet: packet::packet(derivation, key)
            .map(|packet| packet.render())
            .unwrap_or_default(),
    })
}

/// Gives one record to one fresh agent, and reports what it recorded.
#[allow(clippy::too_many_arguments)]
async fn launch(
    project_dir: &Path,
    reviewer: &Reviewer,
    run: &ReviewRun,
    credentials: &Credentials,
    url: &str,
    scratch: &Path,
    job: String,
    target: Target,
) -> ReviewOutcome {
    let by = Party {
        kind: PartyKind::Agent,
        name: reviewer.name.clone(),
        job: Some(job.clone()),
    };

    let token = credentials.issue_detached(CredentialRole::Reviewer, by.clone());

    let handle = AgentHandle {
        task: TaskId::fresh(),
        cancel: CancellationToken::new(),
    };

    // Nothing reads the agents' events: a review's outcome is what the
    // record holds, as a design task's is what the engine committed.
    let (events, _) = tokio::sync::mpsc::unbounded_channel();

    let exit = reviewer
        .backend
        .launch(
            SessionLaunch {
                name: job.clone(),
                prompt: prompt(&target),
                mcp_url: url.to_string(),
                token: token.clone(),
                repo: Some(project_dir.to_path_buf()),
                work_dir: scratch.join(&job),
                budget: run.budget,
            },
            &handle,
            &events,
        )
        .await;

    let recorded = credentials
        .recorded(&token)
        .into_iter()
        .rev()
        .find(|reviewed| reviewed.declaration == target.key);

    let result = match recorded {
        Some(reviewed) => ReviewResult::of(Ok(reviewed)),

        None => ReviewResult::NotReported {
            message: match exit {
                Ok(exit) => format!(
                    "the agent {} without recording a review{}",
                    match exit.status {
                        AgentExitStatus::Completed => "finished".to_string(),
                        AgentExitStatus::Failed { code: Some(code) } => {
                            format!("exited with status {code}")
                        }
                        AgentExitStatus::Failed { code: None } => "failed".to_string(),
                        AgentExitStatus::Cancelled => "was cancelled".to_string(),
                        AgentExitStatus::TimedOut => "ran out of time".to_string(),
                    },
                    exit.final_message
                        .map(|message| format!(": {message}"))
                        .unwrap_or_default()
                ),

                Err(error) => error.to_string(),
            },
        },
    };

    ReviewOutcome {
        declaration: target.key,
        by: Some(by),
        result,
    }
}

/// What an agent is told: the record, the work item, and how to report.
/// The attester's reasoning is nowhere in it.
fn prompt(target: &Target) -> String {
    let mut out = String::new();

    let _ = writeln!(
        out,
        "You are reviewing a Conseqa conformance record against this repository's code, \
         independently of whoever attested it. Do not edit any file.\n"
    );

    let _ = writeln!(out, "Record: `{}`\n", target.path);
    let _ = writeln!(out, "```yaml\n{}```\n", target.text);

    let _ = writeln!(
        out,
        "The declaration's work item follows. Its Code section lists the regions to read, \
         and each fact's evidence is in the record above, by path and line range.\n"
    );

    out.push_str(&target.packet);

    let _ = writeln!(
        out,
        "\nRead the code yourself, then decide:\n\
         - for each attested fact, whether the code at its evidence, inside the \
         declaration's regions, provides it;\n\
         - where the record claims structural conformance, whether its regions implement the \
         residual and do no modeled work beyond it;\n\
         - for each completeness claim, whether its patterns catch every write they must.\n\n\
         Then call the `record_review` tool once, with:\n\
         - declaration: \"{key}\"\n\
         - record: \"{digest}\"\n\
         - verdict: \"confirmed\" or \"disputed\", for the record as a whole\n\
         - items: one for each fact, claim, and the structural claim you can judge, \
         {{\"fact\": <index>, \"verdict\": ..., \"note\": \"...\"}}, \
         {{\"completeness\": <index>, ...}}, or {{\"structural\": true, ...}}\n\n\
         Dispute what you cannot confirm, and say in its note what you saw.",
        key = target.key,
        digest = target.digest,
    );

    out
}
