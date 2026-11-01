//! `conseqa conform` (§15.2 and §15.3 of the conformance and
//! implementation guidance revision; its test §18.11): each subcommand,
//! each exit code, and each gate, in a real checkout.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};

struct Checkout(PathBuf);

impl Checkout {
    /// A git checkout of the flash checkout project, run by Ana, with
    /// the cancellation's code bound.
    fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("conseqa-conform-{label}-{}", uuid::Uuid::new_v4()));

        fs::create_dir_all(path.join("src")).expect("the checkout");

        let checkout = Self(path);

        checkout.git(&["init", "-q"]);
        checkout.git(&["config", "user.name", "Ana Tester"]);
        checkout.git(&["config", "user.email", "ana@example.test"]);
        checkout.git(&["config", "commit.gpgsign", "false"]);

        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/flash_checkout.yaml");

        let output = checkout.conseqa(&["init", "--model", fixture.to_str().unwrap()]);

        assert!(output.status.success(), "{}", stderr(&output));

        checkout.put(
            "src/cancel.rs",
            "// conseqa:begin transaction operation.cancel_order/tx.cancel_order\n\
             pub fn cancel(db: &mut Db, order_id: Uuid) -> Result<()> {\n\
             \x20   let order = db.one(\"SELECT * FROM orders WHERE order_id = $1\", order_id)?;\n\
             \x20   db.execute(\"UPDATE orders SET status = 'cancelled' WHERE order_id = $1\n\
             \x20               AND version = $2\", (order_id, order.version))\n\
             }\n\
             // conseqa:end transaction operation.cancel_order/tx.cancel_order\n",
        );

        checkout
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn git(&self, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.0)
            .args(args)
            .output()
            .expect("git runs");

        assert!(output.status.success(), "git {args:?}: {}", stderr(&output));
    }

    fn put(&self, path: &str, contents: &str) {
        let path = self.0.join(path);

        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn conseqa(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_conseqa"))
            .args(args)
            .current_dir(&self.0)
            .output()
            .expect("run conseqa")
    }

    /// `conseqa conform <args>`, with stdin fed `input`.
    fn conform_with_input(&self, args: &[&str], input: &str) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_conseqa"))
            .arg("conform")
            .args(args)
            .current_dir(&self.0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run conseqa");

        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();

        child.wait_with_output().expect("conseqa ends")
    }

    fn conform(&self, args: &[&str]) -> Output {
        let mut all = vec!["conform"];
        all.extend_from_slice(args);

        self.conseqa(&all)
    }

    /// A conform command's `--json` document.
    fn document(&self, args: &[&str]) -> (Value, i32) {
        let mut all = args.to_vec();
        all.push("--json");

        let output = self.conform(&all);
        let document = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("{error}: {}{}", stdout(&output), stderr(&output)));

        (document, output.status.code().expect("an exit code"))
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

const CANCEL: &str = "transaction operation.cancel_order/tx.cancel_order";

/// An attest of every attestable fact the cancellation owes, evidenced
/// inside its region.
fn cancellation_attest(checkout: &Checkout) -> Value {
    let (packet, status) = checkout.document(&["item", CANCEL]);

    assert_eq!(status, 0);
    assert_eq!(packet["document"], json!("item"));

    let facts: Vec<Value> = packet["required"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|required| required["class"] == json!("attestable"))
        .map(|required| {
            json!({
                "fact": required["fact"],
                "evidence": [{ "path": "src/cancel.rs", "lines": [2, 6] }],
            })
        })
        .collect();

    json!({
        "declaration": CANCEL,
        "facts": facts,
        "structural": false,
        "read_set": packet["read_set"],
    })
}

#[test]
fn status_plan_and_item_derive_and_print_their_documents() {
    let checkout = Checkout::new("reads");

    let output = checkout.conform(&["status"]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(
        stdout(&output).starts_with("declarations"),
        "{}",
        stdout(&output)
    );

    let report = checkout.path().join("status.json");
    let output = checkout.conform(&["status", "--report", report.to_str().unwrap()]);

    assert_eq!(code(&output), 0);

    let written: Value = serde_json::from_str(&fs::read_to_string(&report).unwrap()).unwrap();

    assert_eq!(written["format"], json!(1));
    assert_eq!(written["document"], json!("status"));

    let (plan, status) = checkout.document(&["plan"]);

    assert_eq!(status, 0);
    assert_eq!(plan["document"], json!("plan"));
    assert!(
        plan["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["key"] == json!(CANCEL))
    );

    let output = checkout.conform(&["item", CANCEL]);

    assert_eq!(code(&output), 0);
    assert!(stdout(&output).starts_with(&format!("# {CANCEL}")));

    // A key with no work item.
    let output = checkout.conform(&["item", "transaction operation.nope/tx.nope"]);

    assert_eq!(code(&output), 1);
    assert!(
        stderr(&output).contains("has no work item"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn attest_writes_what_git_says_who_and_refuses_with_exit_two() {
    let checkout = Checkout::new("attest");

    let attest = cancellation_attest(&checkout);
    let input = checkout.path().join("attest.json");

    fs::write(&input, attest.to_string()).unwrap();

    let output = checkout.conform(&["attest", "--input", input.to_str().unwrap()]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(stdout(&output).starts_with("wrote .conseqa/conformance/transaction/"));

    let record = fs::read_to_string(
        checkout
            .path()
            .join(".conseqa/conformance/transaction/operation.cancel_order/tx.cancel_order.yaml"),
    )
    .unwrap();

    assert!(record.contains("kind: person"), "{record}");
    assert!(record.contains("name: Ana Tester"), "{record}");

    // The same attest again, from stdin: nothing to write.
    let (document, status) = {
        let output =
            checkout.conform_with_input(&["attest", "--input", "-", "--json"], &attest.to_string());

        (
            serde_json::from_slice::<Value>(&output.stdout).unwrap(),
            code(&output),
        )
    };

    assert_eq!(status, 0);
    assert_eq!(document["outcome"], json!("unchanged"));

    // Refused, by name, with exit 2.
    let mut wrong = attest.clone();
    wrong["declaration"] = json!("transaction operation.apply_payment/tx.apply_payment");

    let output = checkout.conform_with_input(&["attest", "--input", "-"], &wrong.to_string());

    assert_eq!(code(&output), 2);
    assert!(
        stderr(&output).starts_with("attest refused:"),
        "{}",
        stderr(&output)
    );

    // An input that does not parse is no attest at all.
    let output = checkout.conform_with_input(&["attest", "--input", "-"], "{\"by\": \"me\"}");

    assert_eq!(code(&output), 1);
}

#[test]
fn check_fails_on_scan_errors_and_on_the_gates_named() {
    let checkout = Checkout::new("check");

    // No gate, no scan error: the spec may lead the code.
    let output = checkout.conform(&["check"]);

    assert_eq!(code(&output), 0, "{}", stdout(&output));
    assert!(stdout(&output).ends_with("passed\n"));

    for gate in [
        "not_conformant",
        "missing_facts",
        "unbound",
        "not_conformant,disputed",
    ] {
        let output = checkout.conform(&["check", "--fail-on", gate]);

        assert_eq!(code(&output), 2, "--fail-on {gate}: {}", stdout(&output));
        assert!(
            stdout(&output).contains("gate failed:"),
            "{}",
            stdout(&output)
        );
    }

    for gate in ["disputed", "orphaned", "drifted"] {
        assert_eq!(
            code(&checkout.conform(&["check", "--fail-on", gate])),
            0,
            "{gate}"
        );
    }

    // A gate that is no condition.
    let output = checkout.conform(&["check", "--fail-on", "conformant"]);

    assert_eq!(code(&output), 1);
    assert!(
        stderr(&output).contains("is not a gate"),
        "{}",
        stderr(&output)
    );

    // The check document, printed and written.
    let report = checkout.path().join("check.json");
    let output = checkout.conform(&[
        "check",
        "--fail-on",
        "missing_facts",
        "--report",
        report.to_str().unwrap(),
    ]);

    assert_eq!(code(&output), 2);

    let document: Value = serde_json::from_str(&fs::read_to_string(&report).unwrap()).unwrap();

    assert_eq!(document["document"], json!("check"));
    assert_eq!(document["passed"], json!(false));
    assert_eq!(document["failed"][0]["gate"], json!("missing_facts"));

    // A scan error fails every check, and the scan.
    checkout.put(
        "src/broken.rs",
        "// conseqa:begin transaction operation.cancel_order/tx.cancel_order\n",
    );

    let output = checkout.conform(&["check"]);

    assert_eq!(code(&output), 2);
    assert!(
        stdout(&output).contains("scan error"),
        "{}",
        stdout(&output)
    );

    let output = checkout.conform(&["scan"]);

    assert_eq!(code(&output), 2);
    assert!(
        stdout(&output).contains("1 scan error"),
        "{}",
        stdout(&output)
    );

    fs::remove_file(checkout.path().join("src/broken.rs")).unwrap();

    let (scan, status) = checkout.document(&["scan"]);

    assert_eq!(status, 0);
    assert_eq!(scan["document"], json!("scan"));
    assert_eq!(scan["regions"][0]["key"], json!(CANCEL));
}

#[test]
fn a_state_that_cannot_be_derived_exits_one() {
    let checkout = Checkout::new("underivable");

    // A record that does not parse fails the read.
    checkout.put(
        ".conseqa/conformance/transaction/operation.cancel_order/tx.cancel_order.yaml",
        "format: 1\nnot a record\n",
    );

    for args in [&["status"][..], &["check"][..], &["plan"][..]] {
        let output = checkout.conform(args);

        assert_eq!(code(&output), 1, "{args:?}: {}", stderr(&output));
        assert!(
            stderr(&output).contains("does not load"),
            "{}",
            stderr(&output)
        );
    }

    fs::remove_dir_all(checkout.path().join(".conseqa/conformance")).unwrap();

    // A spec that does not validate has no proofs to hold code to.
    let operation = checkout
        .path()
        .join(".conseqa/spec/operations/operation.cancel_order.yaml");

    let text = fs::read_to_string(&operation).unwrap();

    fs::write(
        &operation,
        text.replacen("data_model: data.checkout", "data_model: data.missing", 1),
    )
    .unwrap();

    let output = checkout.conform(&["check"]);

    assert_eq!(code(&output), 1);
    assert!(
        stderr(&output).contains("cannot be verified"),
        "{}",
        stderr(&output)
    );

    // The scan reads the files alone, so it still runs.
    assert_eq!(code(&checkout.conform(&["scan"])), 0);
}

#[test]
fn a_spec_range_scopes_the_plan_and_the_check_to_its_own_changes() {
    let checkout = Checkout::new("range");

    checkout.git(&["add", "-A"]);
    checkout.git(&["commit", "-q", "-m", "the project"]);

    let operation = checkout
        .path()
        .join(".conseqa/spec/operations/operation.cancel_order.yaml");

    let text = fs::read_to_string(&operation).unwrap();

    fs::write(
        &operation,
        text.replacen("isolation: read_committed", "isolation: serializable", 1),
    )
    .unwrap();

    checkout.git(&["commit", "-q", "-am", "serializable cancellations"]);

    let (whole, _) = checkout.document(&["plan"]);
    let (scoped, status) = checkout.document(&["plan", "--spec-range", "HEAD~1..HEAD"]);

    assert_eq!(status, 0);
    assert_eq!(
        scoped["scope"]["changed"][0]["value"]["transaction"],
        json!("tx.cancel_order")
    );

    let keys = |plan: &Value| -> Vec<String> {
        plan["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["key"].as_str().unwrap().to_string())
            .collect()
    };

    let (whole, scoped) = (keys(&whole), keys(&scoped));

    assert!(scoped.contains(&CANCEL.to_string()), "{scoped:?}");
    assert!(scoped.len() < whole.len(), "{scoped:?} of {whole:?}");
    assert!(scoped.iter().all(|key| whole.contains(key)));

    // The check gates on the same scope: what the range owes.
    let (check, status) = checkout.document(&[
        "check",
        "--spec-range",
        "HEAD~1..HEAD",
        "--fail-on",
        "unbound",
    ]);

    let unbound: Vec<&str> = check["failed"][0]["items"]
        .as_array()
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    assert!(
        unbound.iter().all(|key| scoped.contains(&key.to_string())),
        "{check}"
    );
    assert_eq!(status, if unbound.is_empty() { 0 } else { 2 });

    // A base and a range are alternatives, and a range names commits.
    assert_eq!(
        code(&checkout.conform(&["plan", "HEAD", "--spec-range", "HEAD~1..HEAD"])),
        2,
        "clap refuses conflicting arguments with its own usage error"
    );

    let output = checkout.conform(&["plan", "--spec-range", "no-such..HEAD"]);

    assert_eq!(code(&output), 1, "{}", stderr(&output));
}

fn fake(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fake_agents")
        .join(name)
        .display()
        .to_string()
}

#[test]
fn review_runs_an_agent_per_record_and_exits_two_when_one_is_left_unreviewed() {
    let checkout = Checkout::new("review");

    let attest = cancellation_attest(&checkout);
    let output = checkout.conform_with_input(&["attest", "--input", "-"], &attest.to_string());

    assert_eq!(code(&output), 0, "{}", stderr(&output));

    // An agent that ends without recording a review leaves it so.
    let silent = fake("claude_ok.sh");
    let output = checkout.conform(&[
        "review",
        CANCEL,
        "--backend",
        "claude",
        "--backend-program",
        &silent,
    ]);

    assert_eq!(code(&output), 2, "{}{}", stdout(&output), stderr(&output));
    assert!(
        stdout(&output).contains("not reported"),
        "{}",
        stdout(&output)
    );

    // An agent that reports its review: recorded, by the agent, not by
    // Ana, who attested.
    let reviewer = fake("claude_review.sh");
    let (document, status) = checkout.document(&[
        "review",
        "--backend",
        "claude",
        "--backend-program",
        &reviewer,
        "--model",
        "claude-opus-5-5",
    ]);

    assert_eq!(status, 0, "{document}");
    assert_eq!(document["document"], json!("review"));
    assert_eq!(document["reviews"][0]["outcome"], json!("recorded"));
    assert_eq!(
        document["reviews"][0]["by"]["name"],
        json!("claude/claude-opus-5-5")
    );

    // Nothing is left to review.
    let output = checkout.conform(&[
        "review",
        "--backend",
        "claude",
        "--backend-program",
        &silent,
    ]);

    assert_eq!(code(&output), 0);
    assert!(
        stdout(&output).starts_with("nothing to review"),
        "{}",
        stdout(&output)
    );
}
