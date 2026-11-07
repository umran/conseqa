//! `conseqa realize` and `conseqa check --require-realizable` (§15.1 of
//! the realization profiles revision): each subcommand and each exit
//! code, in a real checkout.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

/// A project entry for the worker runtime the stack names: the only
/// thing in it that bounds a pool member's concurrency.
const NOMAD: &str = "format: 1
id: nomad.job
version: 1
technology: { name: nomad, versions: \">=1.6\" }
realizes: { kind: execution_pool }
provides:
  - concurrency: bounded
requirements:
  - id: task-concurrency
    of: self
    statement: Each task group runs the pool's bound of invocations at once, and no more.
evidence:
  task-concurrency: { look_for: the job's task group }
";

struct Checkout(PathBuf);

impl Checkout {
    /// A checkout of the flash checkout project whose stack realizes
    /// every declaration: PostgreSQL, Kafka, Envoy, and a Nomad
    /// worker runtime the project's own catalogue describes.
    fn new(label: &str) -> Self {
        let checkout = Self::bare(label);

        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/flash_checkout.yaml");
        let output = checkout.conseqa(&["init", "--model", fixture.to_str().unwrap()]);

        assert!(output.status.success(), "{}", stderr(&output));

        checkout.stack(&["postgres@16", "kafka@3.7", "envoy@1.30", "nomad@1.8"]);
        checkout.put(".conseqa/realization/catalogue/nomad.job.yaml", NOMAD);

        checkout
    }

    /// An empty directory, in no project.
    fn bare(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("conseqa-realize-{label}-{}", uuid::Uuid::new_v4()));

        fs::create_dir_all(&path).expect("the checkout");

        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn put(&self, path: &str, contents: &str) {
        let path = self.0.join(path);

        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn stack(&self, technologies: &[&str]) {
        self.put(
            ".conseqa/realization/stack.yaml",
            &format!(
                "format: 1\ncatalogue: conseqa-standard@1\ntechnologies: [{}]\n",
                technologies.join(", ")
            ),
        );
    }

    fn conseqa(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_conseqa"))
            .args(args)
            .current_dir(&self.0)
            .output()
            .expect("run conseqa")
    }

    fn realize(&self, args: &[&str]) -> Output {
        let mut all = vec!["realize"];
        all.extend_from_slice(args);

        self.conseqa(&all)
    }

    /// A realize command's `--json` document, and its exit code.
    fn document(&self, args: &[&str]) -> (Value, i32) {
        let mut all = args.to_vec();
        all.push("--json");

        let output = self.realize(&all);
        let document = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("{error}: {}{}", stdout(&output), stderr(&output)));

        (document, code(&output))
    }
}

impl Drop for Checkout {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn code(output: &Output) -> i32 {
    output.status.code().expect("an exit code")
}

#[test]
fn status_explain_and_the_catalogue_read_the_project() {
    let checkout = Checkout::new("read");

    let output = checkout.realize(&["status", "--require-realizable"]);
    let text = stdout(&output);

    assert_eq!(code(&output), 0, "{text}{}", stderr(&output));
    assert!(text.starts_with(
        "realization against conseqa-standard@1, with envoy@1.30, kafka@3.7, nomad@1.8, \
         postgres@16\n"
    ));

    for line in [
        "automatic      data_model data.checkout",
        "automatic      router router.create_order",
        "unneeded       router router.cancel_order",
        "automatic      execution_pool pool.order_workers",
        "unneeded       execution_pool pool.checkout_api",
        "0 chosen, 12 automatic, 0 ambiguous, 0 invalid choice, 0 infeasible, 4 unneeded",
    ] {
        assert!(text.contains(line), "missing {line:?} in\n{text}");
    }

    let (status, exit) = checkout.document(&["status"]);

    assert_eq!(exit, 0);
    assert_eq!(status["document"], "realization_status");

    let router = status["declarations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|realized| realized["declaration"] == "router router.create_order")
        .unwrap();

    assert_eq!(router["state"], "automatic");
    assert_eq!(router["entry"], "envoy.ring-hash@1");

    let output = checkout.realize(&["explain", "router router.create_order"]);
    let text = stdout(&output);

    assert_eq!(code(&output), 0, "{}", stderr(&output));

    for line in [
        "automatic  router router.create_order",
        "needs assignment: consistent_hash",
        "candidate envoy.ring-hash@1 (standard)",
        "candidate envoy.round-robin@1 (standard)",
        "  cannot provide assignment: consistent_hash: Round robin selects hosts in rotation, \
         whatever key a request carries.",
    ] {
        assert!(text.contains(line), "missing {line:?} in\n{text}");
    }

    let (explanation, exit) = checkout.document(&["explain", "router router.create_order"]);

    assert_eq!(exit, 0);
    assert_eq!(explanation["document"], "realization_explain");

    let output = checkout.realize(&["explain", "router router.nowhere"]);

    assert_eq!(code(&output), 1);
    assert!(stderr(&output).contains("the spec has no declaration `router router.nowhere`"));

    let output = checkout.realize(&["catalogue"]);
    let text = stdout(&output);

    assert_eq!(code(&output), 0);
    assert!(text.contains("nomad.job@1"), "{text}");
    assert!(text.contains("project"), "{text}");
    assert!(text.contains("kafka.consumer-group@1"), "{text}");

    let (listing, exit) = checkout.document(&["catalogue", "list"]);

    assert_eq!(exit, 0);
    assert_eq!(listing["document"], "catalogue");
    assert_eq!(listing["catalogue"], "conseqa-standard@1");

    let output = checkout.realize(&["catalogue", "show", "kafka.keyed-topic@1"]);

    assert_eq!(code(&output), 0);
    assert!(stdout(&output).starts_with("# kafka.keyed-topic@1 (standard)\n"));
    assert!(stdout(&output).contains("single_partition"));

    let (entry, exit) = checkout.document(&["catalogue", "show", "nomad.job"]);

    assert_eq!(exit, 0);
    assert_eq!(entry["document"], "catalogue_entry");
    assert_eq!(entry["source"], "project");

    let output = checkout.realize(&["catalogue", "show", "kafka.keyed-topic@2"]);

    assert_eq!(code(&output), 1);
    assert!(stderr(&output).contains("no catalogue holds `kafka.keyed-topic@2`"));
}

#[test]
fn choose_resolves_what_the_gates_refuse() {
    let checkout = Checkout::new("choose");

    checkout.stack(&[
        "postgres@16",
        "kafka@3.7",
        "envoy@1.30",
        "nomad@1.8",
        "sqs@2012.11.05",
    ]);

    let output = checkout.realize(&["status", "--require-realizable"]);

    assert_eq!(code(&output), 2, "{}", stdout(&output));
    assert!(
        stdout(&output).lines().any(|line| {
            line.split_whitespace().collect::<Vec<_>>().join(" ")
                == "ambiguous topic_runtime topic.order_events kafka.keyed-topic@1 | sqs.fifo@1"
        }),
        "{}",
        stdout(&output)
    );
    assert!(stderr(&output).contains("not every realizable declaration is realizable"));

    // Without the flag, the same status is a report.
    assert_eq!(code(&checkout.realize(&["status"])), 0);

    let output = checkout.conseqa(&["check", "--require-realizable"]);

    assert_eq!(code(&output), 2, "{}{}", stdout(&output), stderr(&output));
    assert!(stdout(&output).contains("realization: 16 realizable declarations, 1 not realizable"));
    assert!(stderr(&output).contains("ambiguous topic_runtime topic.order_events"));

    assert_eq!(code(&checkout.conseqa(&["check"])), 0);

    let output = checkout.realize(&["choose", "topic_runtime topic.order_events", "sqs.standard"]);

    assert_eq!(code(&output), 2);
    assert!(
        stderr(&output).contains(
            "refused: `sqs.standard@1` is not a valid realization: it does not provide \
             `grouping: keyed`"
        ),
        "{}",
        stderr(&output)
    );

    let (refused, exit) = checkout.document(&[
        "choose",
        "topic_runtime topic.order_events",
        "sqs.fifo",
        "--option",
        "single_partition",
    ]);

    assert_eq!(exit, 2);
    assert_eq!(refused["document"], "realization_choice");
    assert_eq!(refused["outcome"], "refused");
    assert_eq!(refused["refusal"], "invalid");
    assert_eq!(refused["problems"][0]["problem"], "unknown_option");

    let output = checkout.realize(&[
        "choose",
        "topic_runtime topic.order_events",
        "kafka.keyed-topic",
        "--reason",
        "The order stream stays on Kafka.",
    ]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "chose kafka.keyed-topic@1 for topic_runtime topic.order_events \
         (.conseqa/realization/choices/topic_runtime/topic.order_events.yaml)\n"
    );

    assert_eq!(
        fs::read_to_string(
            checkout
                .path()
                .join(".conseqa/realization/choices/topic_runtime/topic.order_events.yaml")
        )
        .unwrap(),
        "format: 1\ndeclaration: topic_runtime topic.order_events\nentry: kafka.keyed-topic@1\n\
         reason: The order stream stays on Kafka.\n"
    );

    let (unchanged, exit) = checkout.document(&[
        "choose",
        "topic_runtime topic.order_events",
        "kafka.keyed-topic@1",
        "--reason",
        "The order stream stays on Kafka.",
    ]);

    assert_eq!(exit, 0);
    assert_eq!(unchanged["outcome"], "unchanged");

    assert_eq!(
        code(&checkout.realize(&["status", "--require-realizable"])),
        0
    );
    assert_eq!(
        code(&checkout.conseqa(&["check", "--require-realizable"])),
        0
    );
}

#[test]
fn a_profile_that_does_not_load_exits_1_and_no_stack_passes() {
    let checkout = Checkout::new("load");

    fs::remove_dir_all(checkout.path().join(".conseqa/realization")).unwrap();

    let output = checkout.realize(&["status", "--require-realizable"]);

    assert_eq!(code(&output), 0);
    assert_eq!(
        stdout(&output),
        "no stack profile at .conseqa/realization/stack.yaml; realization does not apply\n"
    );

    let output = checkout.conseqa(&["check", "--require-realizable"]);

    assert_eq!(code(&output), 0);
    assert!(stdout(&output).contains("realization: no stack profile, so nothing to realize"));

    let output = checkout.realize(&["explain", "data_model data.checkout"]);

    assert_eq!(code(&output), 1);
    assert!(stderr(&output).contains("the project has no stack profile"));

    let (refused, exit) =
        checkout.document(&["choose", "data_model data.checkout", "postgres.database"]);

    assert_eq!(exit, 2);
    assert_eq!(refused["refusal"], "no_stack");

    checkout.stack(&["postgres"]);

    for args in [
        &["realize", "status"][..],
        &["realize", "explain", "data_model data.checkout"],
        &["check", "--require-realizable"],
    ] {
        let output = checkout.conseqa(args);

        assert_eq!(code(&output), 1, "{args:?}");
        assert!(
            stderr(&output).contains("`postgres` names no version"),
            "{args:?}: {}",
            stderr(&output)
        );
    }
}

#[test]
fn the_catalogue_reads_outside_a_project() {
    let outside = Checkout::bare("outside");

    let output = outside.realize(&["catalogue", "list"]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(stdout(&output).starts_with("conseqa-standard@1 and the project's entries\n"));
    assert!(stdout(&output).contains("postgres.database@1"));
    assert!(!stdout(&output).contains("project "));
}

impl Checkout {
    fn git(&self, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.0)
            .args(args)
            .output()
            .expect("git runs");

        assert!(output.status.success(), "git {args:?}: {}", stderr(&output));
    }

    /// Commits everything, as the spec's pull request would.
    fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
    }
}

#[test]
fn the_diff_gates_fail_on_what_the_heads_stack_cannot_realize() {
    let checkout = Checkout::new("gates");

    checkout.git(&["init", "-q"]);
    checkout.git(&["config", "user.name", "Ana Tester"]);
    checkout.git(&["config", "user.email", "ana@example.test"]);
    checkout.git(&["config", "commit.gpgsign", "false"]);
    checkout.commit("the checkout, realizable");

    let gates = |checkout: &Checkout| {
        let output = checkout.conseqa(&["diff", "--fail-on", "infeasible,ambiguous"]);

        (code(&output), stdout(&output), stderr(&output))
    };

    let (exit, text, _) = gates(&checkout);

    assert_eq!(exit, 0, "{text}");
    assert!(text.contains("realization\n"), "{text}");

    // SQS joins the stack: the topic runtime needs a choice.
    checkout.stack(&[
        "postgres@16",
        "kafka@3.7",
        "envoy@1.30",
        "nomad@1.8",
        "sqs@2012.11.05",
    ]);

    let (exit, text, errors) = gates(&checkout);

    assert_eq!(exit, 2, "{text}{errors}");
    assert!(text.contains("  + stack sqs@2012.11.05"), "{text}");
    assert!(
        text.contains("! ambiguous     topic_runtime topic.order_events"),
        "{text}"
    );
    assert!(errors.contains("gate failed: ambiguous"), "{errors}");
    assert!(!errors.contains("gate failed: infeasible"), "{errors}");

    let (document, exit) = {
        let output = checkout.conseqa(&["diff", "--report", "diff.json", "--fail-on", "ambiguous"]);

        (
            serde_json::from_slice::<Value>(&fs::read(checkout.path().join("diff.json")).unwrap())
                .unwrap(),
            code(&output),
        )
    };

    assert_eq!(exit, 2);
    assert_eq!(
        document["realization"]["stack"]["added"][0],
        "sqs@2012.11.05"
    );
    assert_eq!(
        document["realization"]["unrealizable"][0]["state"],
        "ambiguous"
    );

    // The worker runtime's entry leaves the catalogue: every bounded pool
    // is infeasible.
    checkout.stack(&["postgres@16", "kafka@3.7", "envoy@1.30", "nomad@1.8"]);
    fs::remove_file(
        checkout
            .path()
            .join(".conseqa/realization/catalogue/nomad.job.yaml"),
    )
    .unwrap();

    let (exit, text, errors) = gates(&checkout);

    assert_eq!(exit, 2, "{text}{errors}");
    assert!(
        text.contains("! infeasible    execution_pool pool.order_workers"),
        "{text}"
    );
    assert!(errors.contains("gate failed: infeasible"), "{errors}");
}
