//! `conseqa-harness decider`: ask a System One decider directly, and
//! vet a server before it is trusted (§11.2, §26 of the System One
//! orchestration revision).
//!
//! Both commands send only synthetic material — three made-up support
//! messages — so neither puts a prompt or a specification on the wire.

use std::process::ExitCode;

use clap::Subcommand;
use serde_json::json;

use conseqa::harness::executors::cli::{BackendArgs, DeciderArgs, announce_egress};
use conseqa::system_one::questions::conformance as wording;
use conseqa::system_one::{Decider, DecisionRequest, conformance};

#[derive(Subcommand)]
pub enum DeciderCommand {
    /// Send one small synthetic request and print the typed answers:
    /// the check that a URL, a model id and a credential work together.
    Probe(DeciderArgs),

    /// Run the conformance suite against a wire-format server. Exits
    /// with 2 when the server is not admissible.
    Conformance(BackendArgs),
}

pub async fn run(command: DeciderCommand) -> Result<ExitCode, String> {
    match command {
        DeciderCommand::Probe(args) => probe(args).await,
        DeciderCommand::Conformance(args) => vet(args).await,
    }
}

const SYNTHETIC: &str = "synthetic probe material";

async fn probe(args: DeciderArgs) -> Result<ExitCode, String> {
    let configured = args
        .settings()
        .build()
        .map_err(|error| error.to_string())?
        .ok_or("no decider is configured: pass --decider system-one or --decider replay")?;

    announce_egress(SYNTHETIC, &configured.egress);

    let (topic, expected) = wording::topic_choice(wording::BILLING, 0);

    let request = DecisionRequest::new(wording::message_state(wording::BILLING))
        .ask("topic", topic)
        .ask("charged_twice", wording::true_noul())
        .ask("asks_for_refund", wording::refund_score())
        .tag("spec", wording::TYPED_SHAPES.tag());

    let decision = configured
        .decider
        .decide(&request)
        .await
        .map_err(|error| error.to_string())?;

    let report = json!({
        "backend": decision.identity,
        "answered_by": decision.answered_by,
        "latency_ms": u64::try_from(decision.latency.as_millis()).unwrap_or(u64::MAX),
        "usage": decision.usage,
        "state": request.state,
        "expected": { "topic": expected, "charged_twice": "yes", "asks_for_refund": "level 2" },
        "answers": decision.answers,
        "shadow": decision.shadow.map(|shadow| json!({
            "backend": shadow.identity,
            "answers": shadow.answers,
            "agreement": shadow.agreement,
        })),
    });

    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
    );

    Ok(ExitCode::SUCCESS)
}

async fn vet(args: BackendArgs) -> Result<ExitCode, String> {
    let server = args
        .settings()
        .build("primary")
        .map_err(|error| error.to_string())?;

    if server.is_egress()
        && let Some(endpoint) = server.identity().endpoint
    {
        announce_egress(SYNTHETIC, &[endpoint]);
    }

    let report = conformance::run(&server).await;

    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
    );

    if report.admissible() {
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!("the server is not admissible as a decider");

        Ok(ExitCode::from(2))
    }
}
