//! The in-process build behind the design MCP server's `build_design`.
//!
//! It is the workflow `conseqa design` runs, with two differences.
//! Every task goes to the System One builders — all of them — and what
//! a builder will not settle is handed back to the caller
//! ([`HandOffBackend`]) instead of to a coding agent; and tasks run one
//! at a time, each against the head its predecessor committed. The
//! coordinator already holds the whole model, so it is better placed to
//! finish what code could not than a fresh worker session would be.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;

use crate::design::DesignEngine;
use crate::design::mcp::{BuildOutcome, DesignBuild, DesignBuilder};

use super::backend::AgentBackend;
use super::{
    HandOffBackend, RunStatus, Scheduler, SchedulerPolicy, Supervisor, Workflow, WorkflowConfig,
};

/// Builds designs in process, one at a time across the server.
pub struct InProcessBuilder {
    /// The builders every task goes to first.
    #[cfg(feature = "system-one")]
    builders: super::executors::cli::Executor,

    /// Where a finished build writes its model, report, and manifest,
    /// overriding each project's own `.conseqa/local/design`.
    design_out: Option<PathBuf>,

    running: Arc<AtomicBool>,
}

impl InProcessBuilder {
    /// Builds with `builders` (see `executors::cli::every_builder`).
    #[cfg(feature = "system-one")]
    pub fn new(builders: super::executors::cli::Executor) -> Self {
        Self {
            builders,
            design_out: None,
            running: Arc::default(),
        }
    }

    /// Builds with no builders, which exist only with the `system-one`
    /// feature: every task is handed back.
    #[cfg(not(feature = "system-one"))]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            design_out: None,
            running: Arc::default(),
        }
    }

    pub fn with_design_out(mut self, design_out: Option<PathBuf>) -> Self {
        self.design_out = design_out;
        self
    }

    /// Where a build of `engine`'s project writes its finalized model,
    /// report, and manifest: derived artifacts, so the project's
    /// uncommitted local directory.
    fn design_dir(&self, engine: &DesignEngine) -> PathBuf {
        self.design_out
            .clone()
            .or_else(|| engine.local_dir().map(|local| local.join("design")))
            .unwrap_or_else(|| std::env::temp_dir().join("conseqa-design"))
    }

    /// The backend of one build: the hand-off, behind the builders. A
    /// builder acts on one engine, and each build has its own project's.
    #[cfg(feature = "system-one")]
    fn backend(&self, engine: &DesignEngine, hand_off: HandOffBackend) -> Arc<dyn AgentBackend> {
        self.builders.wrap(engine, Arc::new(hand_off))
    }

    /// The backend of one build: the hand-off alone.
    #[cfg(not(feature = "system-one"))]
    fn backend(&self, _engine: &DesignEngine, hand_off: HandOffBackend) -> Arc<dyn AgentBackend> {
        Arc::new(hand_off)
    }
}

/// Marks the server free again when a build ends, however it ends.
struct Release(Arc<AtomicBool>);

impl Drop for Release {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl DesignBuilder for InProcessBuilder {
    fn build(
        &self,
        engine: DesignEngine,
        prompt: Option<String>,
    ) -> Result<BoxFuture<'static, DesignBuild>, String> {
        if self
            .running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err("a build is already running on this server".to_string());
        }

        let release = Release(Arc::clone(&self.running));

        let hand_off = HandOffBackend::new(engine.clone());

        // No session is ever started, so no worker needs the endpoint.
        let supervisor = Supervisor::new(
            engine.clone(),
            self.backend(&engine, hand_off.clone()),
            "http://127.0.0.1:0/mcp",
            None,
            std::env::temp_dir().join("conseqa-work"),
        );

        // One task at a time: the builders take milliseconds to
        // seconds, and running them in order keeps every one against
        // the head its predecessor committed.
        let scheduler = Scheduler::new(
            engine.clone(),
            supervisor,
            SchedulerPolicy {
                max_concurrent_agents: 1,
                // One retry covers a builder that lost an OCC race.
                max_attempts: 2,
                ..Default::default()
            },
        );

        let workflow = Workflow::new(
            scheduler,
            WorkflowConfig {
                out_dir: self.design_dir(&engine),
                analysis_timeout: Duration::from_secs(180),
                max_iterations: 8,
                prompt,
            },
        )
        .with_halt(hand_off.halt());

        let started_at_revision = engine.head_revision();

        // Spawned, so a client that gives up on the call does not drop
        // the build halfway.
        let run = tokio::spawn(async move {
            let _release = release;
            let started = Instant::now();
            let result = workflow.run().await;

            (result, started.elapsed())
        });

        Ok(Box::pin(async move {
            let (result, elapsed) = match run.await {
                Ok((result, elapsed)) => (result.map_err(|error| error.to_string()), elapsed),
                Err(error) => (
                    Err(format!("the build did not finish: {error}")),
                    Duration::ZERO,
                ),
            };

            let hand_offs = hand_off.handed();

            let outcome = match result {
                Err(error) => BuildOutcome::Failed { error },

                Ok(_) if !hand_offs.is_empty() => BuildOutcome::HandedBack {
                    hand_offs: hand_offs
                        .iter()
                        .map(|hand_off| {
                            serde_json::to_value(hand_off).expect("a hand-off serializes")
                        })
                        .collect(),
                },

                Ok(report) => match report.status {
                    RunStatus::Success { .. } => BuildOutcome::Success,
                    RunStatus::Incomplete {
                        reason, unresolved, ..
                    } => BuildOutcome::Incomplete { reason, unresolved },
                },
            };

            DesignBuild {
                started_at_revision,
                elapsed,
                outcome,
            }
        }))
    }
}
