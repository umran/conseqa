//! Background Conseqa analysis: assemble → validate → verify →
//! summarize, off the commit path (§36–§39 of the confluence spec).
//!
//! Commits publish immediately and enqueue analysis; a dedicated
//! worker thread runs the synchronous checker. Analysis is tagged with
//! its exact revision — a proof of revision R is never presented as
//! the proof state of R+1 — and rapid revisions coalesce: a queued,
//! unpinned analysis is dropped when a newer head arrives, while
//! pinned revisions (repair, finalization, explicit waits) always run.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use parking_lot::{Condvar, Mutex};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::analyzer::{self, Diagnostic, report::ProverReport, verification::VerificationReport};
use crate::spec::{Id, Revision};

use super::events::{EngineEvent, EventBus};
use super::snapshot::WorkspaceSnapshot;
use super::summary::{OperationSummary, derive_summaries};
use super::workspace::AssemblyGap;

/// The analysis state of one exact workspace revision.
#[derive(Debug, Clone)]
pub enum AnalysisState {
    /// Queued; nothing has run.
    Pending,

    /// The revision cannot become a `Model`; validation and
    /// verification are not meaningful. Terminal.
    NotAssemblable { gaps: Vec<AssemblyGap> },

    Validating,

    /// Structural validation failed; verification did not run.
    /// Terminal.
    ValidationFailed { errors: Vec<AnalysisDiagnostic> },

    Verifying,

    /// Verified, with derived summaries. Terminal.
    Ready(Arc<AnalysisSnapshot>),
}

impl AnalysisState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::NotAssemblable { .. } | Self::ValidationFailed { .. } | Self::Ready(_)
        )
    }

    /// A short state label for reports.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::NotAssemblable { .. } => "not_assemblable",
            Self::Validating => "validating",
            Self::ValidationFailed { .. } => "validation_failed",
            Self::Verifying => "verifying",
            Self::Ready(_) => "ready",
        }
    }
}

/// One rendered validation diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalysisDiagnostic {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,

    pub message: String,

    /// True when the code identifies the faulty declaration as an L1
    /// one, so the repair belongs to the topology author rather than
    /// to any operation. False for codes raised from either layer —
    /// the coordinator disambiguates those from the subject.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub runtime: bool,
}

impl From<Diagnostic> for AnalysisDiagnostic {
    fn from(diagnostic: Diagnostic) -> Self {
        let mut message = diagnostic.message;

        for evidence in diagnostic.evidence {
            message.push_str("; ");

            if let Some(subject) = evidence.subject {
                message.push_str(&format!("[{subject}] "));
            }

            message.push_str(&evidence.message);
        }

        Self {
            subject: diagnostic.subject.map(|id| id.to_string()),
            message,
            runtime: diagnostic.code.is_runtime(),
        }
    }
}

/// The complete analysis result of one verified revision.
#[derive(Debug)]
pub struct AnalysisSnapshot {
    pub revision: Revision,
    pub verification: VerificationReport,
    pub obligations: ProverReport,
    pub summaries: BTreeMap<Id, OperationSummary>,
}

struct HubQueue {
    entries: VecDeque<Arc<WorkspaceSnapshot>>,
    shutdown: bool,
}

struct HubInner {
    states: Mutex<FxHashMap<u64, watch::Sender<AnalysisState>>>,
    pins: Mutex<FxHashMap<u64, usize>>,
    queue: Mutex<HubQueue>,
    available: Condvar,
    events: EventBus,
}

impl HubInner {
    fn set_state(&self, revision: Revision, state: AnalysisState) {
        let terminal = state.is_terminal();

        {
            let mut states = self.states.lock();

            let sender = states
                .entry(revision.0)
                .or_insert_with(|| watch::channel(AnalysisState::Pending).0);

            sender.send_replace(state);
        }

        if terminal {
            self.events.emit(EngineEvent::AnalysisReady { revision });
        }
    }

    fn watch(&self, revision: Revision) -> watch::Receiver<AnalysisState> {
        self.states
            .lock()
            .entry(revision.0)
            .or_insert_with(|| watch::channel(AnalysisState::Pending).0)
            .subscribe()
    }

    fn pinned(&self, revision: u64) -> bool {
        self.pins.lock().get(&revision).copied().unwrap_or(0) > 0
    }
}

/// Handle to the background analysis worker. Owned by the engine; its
/// drop shuts the worker down.
pub struct AnalysisHub {
    inner: Arc<HubInner>,
}

impl AnalysisHub {
    pub fn start(events: EventBus) -> Self {
        let inner = Arc::new(HubInner {
            states: Mutex::new(FxHashMap::default()),
            pins: Mutex::new(FxHashMap::default()),
            queue: Mutex::new(HubQueue {
                entries: VecDeque::new(),
                shutdown: false,
            }),
            available: Condvar::new(),
            events,
        });

        {
            let inner = Arc::clone(&inner);

            std::thread::Builder::new()
                .name("conseqa-analysis".to_string())
                .spawn(move || analysis_worker(inner))
                .expect("spawning the analysis worker succeeds");
        }

        Self { inner }
    }

    /// Queues a revision for analysis, coalescing away queued
    /// revisions that are not pinned (§39). The running analysis is
    /// never interrupted.
    pub fn enqueue(&self, snapshot: Arc<WorkspaceSnapshot>) {
        let revision = snapshot.revision;

        {
            let mut queue = self.inner.queue.lock();

            queue
                .entries
                .retain(|queued| self.inner.pinned(queued.revision.0));

            queue.entries.push_back(snapshot);
        }

        self.inner.set_state(revision, AnalysisState::Pending);
        self.inner.available.notify_one();
    }

    /// The current analysis state of a revision. `Pending` for a
    /// revision the hub has never seen.
    pub fn state(&self, revision: Revision) -> AnalysisState {
        self.inner.watch(revision).borrow().clone()
    }

    /// Waits until the revision's analysis reaches a terminal state.
    /// The revision is pinned while awaited, so coalescing cannot drop
    /// it.
    pub async fn ready(&self, revision: Revision) -> AnalysisState {
        let _pin = self.pin(revision);

        let mut receiver = self.inner.watch(revision);

        loop {
            let current = receiver.borrow_and_update().clone();

            if current.is_terminal() {
                return current;
            }

            if receiver.changed().await.is_err() {
                return receiver.borrow().clone();
            }
        }
    }

    /// Pins a revision: its queued analysis survives coalescing until
    /// the guard drops.
    pub fn pin(&self, revision: Revision) -> AnalysisPin {
        *self.inner.pins.lock().entry(revision.0).or_insert(0) += 1;

        AnalysisPin {
            inner: Arc::clone(&self.inner),
            revision: revision.0,
        }
    }
}

impl Drop for AnalysisHub {
    fn drop(&mut self) {
        self.inner.queue.lock().shutdown = true;
        self.inner.available.notify_all();
    }
}

/// Keeps one revision's analysis from being coalesced away.
pub struct AnalysisPin {
    inner: Arc<HubInner>,
    revision: u64,
}

impl Drop for AnalysisPin {
    fn drop(&mut self) {
        let mut pins = self.inner.pins.lock();

        if let Some(count) = pins.get_mut(&self.revision) {
            *count -= 1;

            if *count == 0 {
                pins.remove(&self.revision);
            }
        }
    }
}

fn analysis_worker(inner: Arc<HubInner>) {
    loop {
        let snapshot = {
            let mut queue = inner.queue.lock();

            loop {
                if queue.shutdown {
                    return;
                }

                if let Some(snapshot) = queue.entries.pop_front() {
                    break snapshot;
                }

                inner.available.wait(&mut queue);
            }
        };

        analyze(&inner, &snapshot);
    }
}

/// The pipeline (§37): assemble; if impossible, `NotAssemblable`;
/// else validate; if validation succeeds, verify and derive
/// summaries.
fn analyze(inner: &HubInner, snapshot: &WorkspaceSnapshot) {
    let revision = snapshot.revision;

    let model = match snapshot.workspace.assemble_model() {
        Err(error) => {
            inner.set_state(revision, AnalysisState::NotAssemblable { gaps: error.gaps });

            return;
        }

        Ok(model) => model,
    };

    inner.set_state(revision, AnalysisState::Validating);

    let errors = analyzer::validate(&model);

    if !errors.is_empty() {
        inner.set_state(
            revision,
            AnalysisState::ValidationFailed {
                errors: errors
                    .into_iter()
                    .map(|error| AnalysisDiagnostic::from(Diagnostic::from(error)))
                    .collect(),
            },
        );

        return;
    }

    inner.set_state(revision, AnalysisState::Verifying);

    let verification = analyzer::verification::verify(&model);
    let obligations = analyzer::report::obligations(&model, &verification);
    let summaries = derive_summaries(&model, &verification);

    inner.set_state(
        revision,
        AnalysisState::Ready(Arc::new(AnalysisSnapshot {
            revision,
            verification,
            obligations,
            summaries,
        })),
    );
}
