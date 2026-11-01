//! The work list, work items, attest, reviews, gates, and documents
//! (§11–§15 of the conformance and implementation guidance revision;
//! its tests §18.5 and §18.7–§18.10).

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use serde_json::{Value, json};

use conseqa::{
    analyzer::{
        basis::{ConformanceClass, DeclarationKey, Fact, TextualKey, read_canonical},
        report,
    },
    conformance::{
        ConformanceError,
        attest::{self, AttestError, AttestInput, Attested, Refusal},
        check::{self, Gate},
        document, markers,
        packet::{self, LoadBearing, Packet, ReadSetChange},
        plan::{self, Item, Plan, Priority, Scope},
        record::{
            AttestedFact, ClaimScope, CompletenessClaim, Party, PartyKind, Record, RecordedRegion,
            Records, Review, ReviewItem, ReviewTarget, Span, Verdict,
        },
        review::{self as reviews, ReviewError, ReviewInput, ReviewRefusal, Reviewed},
        state::{self, ConditionKind, Derivation, State},
    },
    design::{
        RunId, RunMetadata, WorkspaceState,
        change::{
            mutation,
            partition::Partition,
            side::{Side, SideName, Source},
        },
    },
    parser::yaml,
};

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "conseqa-conformance-work-{label}-{}",
        uuid::Uuid::new_v4()
    ));

    fs::create_dir_all(&dir).unwrap();

    dir
}

/// Writes `contents` at `path` below `dir`, making its directories.
fn put(dir: &Path, path: &str, contents: impl AsRef<[u8]>) {
    let path = dir.join(path);

    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn spec(text: &str) -> WorkspaceState {
    let model = yaml::parse(text).unwrap();

    WorkspaceState::from_model(&model, RunMetadata::new(RunId("spec".to_string())))
}

fn key(text: &str) -> TextualKey {
    text.parse().unwrap()
}

fn party(kind: PartyKind, name: &str) -> Party {
    Party {
        kind,
        name: name.to_string(),
        job: None,
    }
}

fn attester() -> Party {
    party(PartyKind::Agent, "attester")
}

fn reviewer() -> Party {
    party(PartyKind::Agent, "reviewer")
}

// ---------------------------------------------------------------------
// The wallet: a debit and a charge, each a guarded read-modify-write of
// an account, the charge requesting the debit first.
// ---------------------------------------------------------------------

const HEAD: &str = r#"
dsl: 6
revision: 1
services:
  service.wallet:
    kind: backend
schemas:
  schema.DebitRequest:
    kind: canonical
    completeness: complete
    fields:
      account_id: uuid
      amount: int
  schema.DebitResult:
    kind: canonical
    completeness: complete
    fields:
      account_id: uuid
  schema.Account:
    kind: canonical
    completeness: complete
    fields:
      account_id: uuid
      balance: int
      version: int
data_models:
  data.wallet:
    objects:
      object.account:
        schema: schema.Account
        identity:
        - account_id
        version:
          field: version
topics: {}
state_machines: {}
operations:
"#;

/// The debit operation, a template for the charge.
const OPERATION: &str = r#"  operation.debit:
    service: service.wallet
    inputs:
      input.debit.request:
        kind: request
        schema: schema.DebitRequest
        identity:
          kind: unspecified
        result:
          ok: schema.DebitResult
    program:
      steps:
      - kind: transaction
        transaction:
          id: tx.debit
          data_model: data.wallet
          isolation: ISOLATION
          idempotency:
            kind: unspecified
          requirements:
            serializability:
            - key:
                source:
                  kind: input
                  id: input.debit.request
                path:
                - account_id
            ordering: []
          steps:
          - kind: read
            bind: read.debit.account
            target:
              object: object.account
              predicate:
                kind: eq
                field:
                - account_id
                value:
                  kind: value
                  value:
                    source:
                      kind: input
                      id: input.debit.request
                    path:
                    - account_id
            fields:
              kind: all
          - kind: compare_and_set
            target:
              object: object.account
              predicate:
                kind: eq
                field:
                - account_id
                value:
                  kind: value
                  value:
                    source:
                      kind: input
                      id: input.debit.request
                    path:
                    - account_id
            compare:
            - field: version
              expected:
                source:
                  kind: transaction_read
                  id: read.debit.account
                path:
                - version
            fields:
            - - balance
            values:
              kind: deterministic
              from:
              - source:
                  kind: transaction_read
                  id: read.debit.account
                path:
                - balance
              - source:
                  kind: input
                  id: input.debit.request
                path:
                - amount
        rejected:
          steps:
          - kind: complete
      - kind: return
        request: input.debit.request
        outcome:
          kind: ok
          values:
            kind: deterministic
            from:
            - source:
                kind: input
                id: input.debit.request
              path:
              - account_id
    requirements:
      idempotency: []
      recoverability: []
"#;

/// The step `charge` begins with: a request to the debit.
const REQUEST_DEBIT: &str = r#"      - kind: execute_effect
        effect_id: effect.charge.debit
        effect:
          kind: request
          target:
            operation: operation.debit
            input: input.debit.request
          schema: schema.DebitRequest
          retry: unspecified
          idempotency_key_propagation: []
        values:
          kind: deterministic
          from:
          - source:
              kind: input
              id: input.charge.request
            path:
            - account_id
          - source:
              kind: input
              id: input.charge.request
            path:
            - amount
"#;

const DEBIT: &str = "transaction operation.debit/tx.debit";
const CHARGE: &str = "transaction operation.charge/tx.charge";
const CHARGE_SITE: &str = "effect_site operation.charge/effect.charge.debit";
const DEBIT_PROGRAM: &str = "program operation.debit";
const CHARGE_PROGRAM: &str = "program operation.charge";
const ACCOUNT: &str = "data_object data.wallet/object.account";
const DEBIT_SERIALIZABLE: &str = "SerializableBy(input.debit.request.account_id) on tx.debit";
const CHARGE_SERIALIZABLE: &str = "SerializableBy(input.charge.request.account_id) on tx.charge";

const ACCOUNT_WRITERS: [&str; 1] = [r"(?i)\b(update|delete\s+from|insert\s+into)\s+accounts\b"];

/// The wallet, with the debit at `isolation`. The charge is a guarded
/// read-modify-write like the debit, which first requests the debit:
/// its operation calls the debit's, and sorts before it by key.
fn wallet(isolation: &str) -> WorkspaceState {
    let debit = OPERATION.replace("ISOLATION", isolation);
    let charge = OPERATION
        .replace("ISOLATION", "read_committed")
        .replace("debit", "charge")
        .replacen(
            "      steps:\n      - kind: transaction\n",
            &format!("      steps:\n{REQUEST_DEBIT}      - kind: transaction\n"),
            1,
        );

    spec(&format!("{HEAD}{charge}{debit}"))
}

/// The wallet's code: the debit and the charge, and the accounts table.
fn wallet_code(dir: &Path) {
    put(
        dir,
        "src/wallet.rs",
        "use crate::db::Db;\n\
         \n\
         // conseqa:begin transaction operation.debit/tx.debit\n\
         pub fn debit(db: &mut Db, account_id: Uuid, amount: i64) -> Result<()> {\n\
         \x20   let account = db.one(\"SELECT * FROM accounts WHERE account_id = $1\", account_id)?;\n\
         \x20   db.execute(\n\
         \x20       \"UPDATE accounts SET balance = $1, version = version + 1\n\
         \x20        WHERE account_id = $2 AND version = $3\",\n\
         \x20       (account.balance - amount, account_id, account.version),\n\
         \x20   )\n\
         }\n\
         // conseqa:end transaction operation.debit/tx.debit\n\
         \n\
         // conseqa:begin transaction operation.charge/tx.charge\n\
         pub fn charge(db: &mut Db, account_id: Uuid, amount: i64) -> Result<()> {\n\
         \x20   let account = db.one(\"SELECT * FROM accounts WHERE account_id = $1\", account_id)?;\n\
         \x20   db.execute(\n\
         \x20       \"UPDATE accounts SET balance = $1, version = version + 1\n\
         \x20        WHERE account_id = $2 AND version = $3\",\n\
         \x20       (account.balance + amount, account_id, account.version),\n\
         \x20   )\n\
         }\n\
         // conseqa:end transaction operation.charge/tx.charge\n",
    );

    put(
        dir,
        "migrations/0001_accounts.sql",
        "-- conseqa:begin data_object data.wallet/object.account\n\
         CREATE TABLE accounts (account_id uuid PRIMARY KEY, balance bigint, version bigint);\n\
         -- conseqa:end data_object data.wallet/object.account\n",
    );
}

fn derive<'a>(dir: &Path, workspace: &'a WorkspaceState) -> Derivation<'a> {
    Derivation::of(dir, workspace).unwrap()
}

fn items(plan: &Plan) -> Vec<(String, Priority)> {
    plan.items
        .iter()
        .map(|item| (item.key.to_string(), item.priority))
        .collect()
}

fn item<'a>(plan: &'a Plan, text: &str) -> &'a Item {
    plan.item(&key(text))
        .unwrap_or_else(|| panic!("`{text}` is an item"))
}

fn texts<T: ToString>(values: &[T]) -> Vec<String> {
    values.iter().map(ToString::to_string).collect()
}

fn packet_of(derivation: &Derivation, text: &str) -> Packet {
    packet::packet(derivation, &key(text)).unwrap_or_else(|| panic!("`{text}` has a packet"))
}

/// The facts of one class required of a declaration.
fn required(state: &State, text: &str, class: ConformanceClass) -> Vec<Fact> {
    state
        .declaration(&key(text))
        .unwrap()
        .required
        .iter()
        .filter(|required| required.class == class)
        .map(|required| required.fact.clone())
        .collect()
}

fn kinds(state: &State, text: &str) -> Vec<ConditionKind> {
    state
        .declaration(&key(text))
        .unwrap()
        .conditions
        .iter()
        .map(|condition| condition.kind())
        .collect()
}

fn writers(patterns: &[&str]) -> CompletenessClaim {
    CompletenessClaim {
        scope: ClaimScope::Writers,
        patterns: patterns.iter().map(|pattern| pattern.to_string()).collect(),
        exceptions: Vec::new(),
    }
}

/// The span strictly inside a declaration's first region.
fn inside(dir: &Path, text: &str) -> Span {
    let scan = markers::scan(dir).unwrap();
    let declaration = key(text);

    let region = scan
        .regions_of(declaration.key())
        .next()
        .unwrap_or_else(|| panic!("`{text}` is bound"));

    Span {
        path: region.path.clone(),
        lines: [region.lines[0] + 1, region.lines[1] - 1],
    }
}

/// An attest of `facts`, each evidenced inside the declaration's first
/// region.
fn input(dir: &Path, text: &str, facts: &[Fact]) -> AttestInput {
    let span = inside(dir, text);

    AttestInput {
        declaration: key(text),
        facts: facts
            .iter()
            .map(|fact| AttestedFact {
                fact: fact.clone(),
                evidence: vec![span.clone()],
            })
            .collect(),
        structural: false,
        completeness: Vec::new(),
        read_set: None,
    }
}

fn attest_as(
    dir: &Path,
    workspace: &WorkspaceState,
    input: &AttestInput,
    by: Party,
) -> Result<Attested, AttestError> {
    attest::attest(dir, workspace, input, by)
}

/// The refusal an attest met, by name.
fn refusal(result: Result<Attested, AttestError>) -> Refusal {
    match result {
        Err(AttestError::Refused(refusal)) => *refusal,
        Err(AttestError::Failed(error)) => panic!("the attest failed: {error}"),
        Ok(attested) => panic!("the attest was not refused: {attested:?}"),
    }
}

fn refusal_name(refusal: &Refusal) -> String {
    serde_json::to_value(refusal).unwrap()["refusal"]
        .as_str()
        .unwrap()
        .to_string()
}

fn record_of(dir: &Path, text: &str) -> Record {
    match Records::of_project(dir).read(&key(text)).unwrap() {
        Some(file) => match file.content {
            conseqa::conformance::record::RecordContent::Read(record) => *record,
            other => panic!("`{text}`'s record is set aside: {other:?}"),
        },

        None => panic!("`{text}` has a record"),
    }
}

/// Adds a review to a declaration's record, at its digests.
fn review(dir: &Path, text: &str, review: Review) {
    let records = Records::of_project(dir);
    let file = records.read(&key(text)).unwrap().unwrap();
    let mut record = record_of(dir, text);

    record.reviews.push(review);
    records.write(&record, Some(file.digest)).unwrap();
}

fn confirming(dir: &Path, text: &str, by: Party) -> Review {
    Review {
        by,
        digests: record_of(dir, text).digests(),
        verdict: Verdict::Confirmed,
        items: Vec::new(),
    }
}

/// Attests the debit and the account so that both are current, at
/// `isolation`, each confirmed by the reviewer.
fn attest_debit(dir: &Path, workspace: &WorkspaceState) {
    let state = state::derive(dir, workspace).unwrap();
    let facts = required(&state, DEBIT, ConformanceClass::Attestable);

    attest_as(dir, workspace, &input(dir, DEBIT, &facts), attester()).unwrap();
    review(dir, DEBIT, confirming(dir, DEBIT, reviewer()));

    let mut account = input(dir, ACCOUNT, &[]);
    account.completeness = vec![writers(&ACCOUNT_WRITERS)];

    attest_as(dir, workspace, &account, attester()).unwrap();
    review(dir, ACCOUNT, confirming(dir, ACCOUNT, reviewer()));
}

// ---------------------------------------------------------------------
// §18.7 The work list
// ---------------------------------------------------------------------

#[test]
fn items_come_by_priority_then_in_dependency_order() {
    let dir = temp_dir("order");
    wallet_code(&dir);

    let workspace = wallet("read_committed");
    let derivation = derive(&dir, &workspace);
    let plan = plan::plan(&derivation);

    // Correctness first: the account's absent claim and both guarded
    // mutations block the proofs. Then structure, in dependency order:
    // data before transactions and effect sites, before programs; and
    // the debit, which the charge calls, before the charge, though the
    // charge sorts first by key.
    assert_eq!(
        items(&plan),
        [
            (ACCOUNT.to_string(), Priority::Correctness),
            (DEBIT.to_string(), Priority::Correctness),
            (CHARGE.to_string(), Priority::Correctness),
            (CHARGE_SITE.to_string(), Priority::Structure),
            (CHARGE_PROGRAM.to_string(), Priority::Structure),
            (DEBIT_PROGRAM.to_string(), Priority::Structure),
        ]
    );

    let debit = item(&plan, DEBIT);

    assert_eq!(
        debit.conditions,
        [ConditionKind::Unattested, ConditionKind::MissingFacts]
    );

    // The charge's closure holds the debit, so the debit blocks both.
    assert_eq!(
        texts(&debit.blocks),
        [CHARGE_SERIALIZABLE, DEBIT_SERIALIZABLE]
    );

    assert_eq!(item(&plan, CHARGE_PROGRAM).blocks, []);
}

#[test]
fn an_item_depends_on_the_earlier_items_it_references_and_names_its_files() {
    let dir = temp_dir("depends");
    wallet_code(&dir);

    // The charge's program is bound; its effect site is not.
    put(
        &dir,
        "src/charge.rs",
        "// conseqa:begin program operation.charge\n\
         pub fn charge_handler() {}\n\
         // conseqa:end program operation.charge\n",
    );

    let workspace = wallet("read_committed");
    let derivation = derive(&dir, &workspace);
    let plan = plan::plan(&derivation);

    // A transaction depends on the object it accesses, an effect site
    // on the transactions of the operation it calls, and a program on
    // its own transactions and effect sites.
    assert_eq!(texts(&item(&plan, DEBIT).depends_on), [ACCOUNT]);
    assert_eq!(texts(&item(&plan, CHARGE_SITE).depends_on), [DEBIT]);
    assert_eq!(texts(&item(&plan, DEBIT_PROGRAM).depends_on), [DEBIT]);
    assert_eq!(item(&plan, ACCOUNT).depends_on, Vec::<TextualKey>::new());

    // Its regions' files, and, while it has none, its container's.
    assert_eq!(item(&plan, DEBIT).files, ["src/wallet.rs"]);
    assert_eq!(item(&plan, CHARGE_SITE).files, ["src/charge.rs"]);
    assert_eq!(item(&plan, DEBIT_PROGRAM).files, Vec::<String>::new());

    // Every dependency comes before its dependent in dependency order,
    // so the relation has no cycle.
    for item in &plan.items {
        for dependency in &item.depends_on {
            assert!(
                plan.item(dependency).is_some(),
                "`{}` depends on an item",
                item.key
            );
        }
    }
}

#[test]
fn item_ids_and_the_work_list_are_stable_across_derivations() {
    let dir = temp_dir("stable");
    wallet_code(&dir);

    let workspace = wallet("read_committed");

    let first = serde_json::to_string(&plan::plan(&derive(&dir, &workspace))).unwrap();
    let second = serde_json::to_string(&plan::plan(&derive(&dir, &workspace))).unwrap();

    assert_eq!(first, second);

    // An item's id is its key, until its conditions clear.
    let before = plan::plan(&derive(&dir, &workspace));

    attest_debit(&dir, &workspace);

    let after = plan::plan(&derive(&dir, &workspace));

    assert!(before.item(&key(DEBIT)).is_some());
    assert!(after.item(&key(DEBIT)).is_none());

    let kept: Vec<&TextualKey> = after.items.iter().map(|item| &item.key).collect();

    for key in kept {
        assert!(before.item(key).is_some(), "`{key}` had the same id before");
    }

    // Packets too (§18.10).
    let derivation = derive(&dir, &workspace);

    assert_eq!(
        serde_json::to_string(&packet_of(&derivation, CHARGE)).unwrap(),
        serde_json::to_string(&packet_of(&derive(&dir, &workspace), CHARGE)).unwrap()
    );
}

fn side(name: SideName, workspace: WorkspaceState) -> Side {
    Side {
        name,
        source: Source::WorkingTree,
        commit: None,
        merge_base: false,
        workspace: Some(workspace),
    }
}

#[test]
fn a_diff_scopes_the_work_list_to_the_code_work_its_changes_owe() {
    let dir = temp_dir("scope");
    wallet_code(&dir);

    let committed = wallet("read_committed");
    attest_debit(&dir, &committed);

    let head = wallet("serializable");
    let derivation = derive(&dir, &head);
    let plan = plan::plan(&derivation);

    let scope = Scope::between(
        &side(SideName::Base, committed.clone()),
        &side(SideName::Head, head.clone()),
    );

    // The debit changed, and with it the basis of its proof.
    assert_eq!(
        texts(
            &scope
                .changed
                .iter()
                .cloned()
                .filter_map(TextualKey::new)
                .collect::<Vec<_>>()
        ),
        [DEBIT]
    );
    assert!(
        texts(&scope.moved.iter().cloned().collect::<Vec<_>>())
            .contains(&DEBIT_SERIALIZABLE.to_string())
    );

    let scoped = plan.scoped(&scope, &derivation);

    // Every item kept is a changed declaration's, or owes a fact of a
    // moved obligation's basis: anchors it, implements it, or carries a
    // claim it requires.
    let owing: BTreeSet<DeclarationKey> = scope
        .facts
        .iter()
        .flat_map(|basis_fact| {
            let mut owing: Vec<DeclarationKey> = basis_fact.anchors.iter().cloned().collect();

            owing.extend(state::implementers(
                &basis_fact.fact,
                &head,
                &derivation.graph,
            ));
            owing.extend(
                conseqa::conformance::completeness::scopes(&basis_fact.fact, &head)
                    .into_iter()
                    .map(|(carrier, _)| carrier.into_key()),
            );

            owing
        })
        .chain(scope.changed.iter().cloned())
        .collect();

    let expected: Vec<String> = plan
        .items
        .iter()
        .filter(|item| owing.contains(item.key.key()))
        .map(|item| item.key.to_string())
        .collect();

    assert_eq!(
        items(&scoped)
            .into_iter()
            .map(|(key, _)| key)
            .collect::<Vec<_>>(),
        expected
    );
    assert!(scoped.item(&key(DEBIT)).is_some());

    // The programs are this branch's leading spec's debt elsewhere.
    assert!(plan.item(&key(CHARGE_PROGRAM)).is_some());
    assert!(scoped.item(&key(CHARGE_PROGRAM)).is_none());

    // With no change, nothing is in scope.
    let unchanged = Scope::between(
        &side(SideName::Base, head.clone()),
        &side(SideName::Head, head.clone()),
    );

    assert_eq!(plan.scoped(&unchanged, &derivation).items, []);
}

/// The wallet, its debit declaring no serializability requirement.
fn wallet_without_the_debits_requirement() -> WorkspaceState {
    let requirement = "            serializability:\n\
                       \x20           - key:\n\
                       \x20               source:\n\
                       \x20                 kind: input\n\
                       \x20                 id: input.debit.request\n\
                       \x20               path:\n\
                       \x20               - account_id\n";

    assert!(OPERATION.contains(requirement));

    let debit = OPERATION
        .replace("ISOLATION", "read_committed")
        .replace(requirement, "            serializability: []\n");

    let charge = OPERATION
        .replace("ISOLATION", "read_committed")
        .replace("debit", "charge")
        .replacen(
            "      steps:\n      - kind: transaction\n",
            &format!("      steps:\n{REQUEST_DEBIT}      - kind: transaction\n"),
            1,
        );

    spec(&format!("{HEAD}{charge}{debit}"))
}

#[test]
fn an_added_requirement_scopes_in_the_code_its_proof_needs() {
    let dir = temp_dir("added");
    wallet_code(&dir);

    let head = wallet("read_committed");
    let derivation = derive(&dir, &head);

    // Adding a requirement changes the requirement alone: no member
    // with code changed. The obligation it adds is what scopes the
    // debit in.
    let scope = Scope::between(
        &side(SideName::Base, wallet_without_the_debits_requirement()),
        &side(SideName::Head, head.clone()),
    );

    assert!(
        scope
            .changed
            .iter()
            .all(|declaration| TextualKey::new(declaration.clone()).is_none()),
        "{:?}",
        scope.changed
    );
    assert_eq!(
        texts(&scope.moved.iter().cloned().collect::<Vec<_>>()),
        [DEBIT_SERIALIZABLE]
    );

    let scoped = plan::plan(&derivation).scoped(&scope, &derivation);

    assert!(scoped.item(&key(DEBIT)).is_some(), "{:?}", items(&scoped));
}

// ---------------------------------------------------------------------
// §18.8 Packets
// ---------------------------------------------------------------------

/// The obligations a fact's withdrawal regresses, verified apart from
/// the packet.
fn regressions(workspace: &WorkspaceState, fact: &Fact) -> Vec<String> {
    let before = verified(workspace);
    let after = verified(&mutation::withdraw(workspace, fact).expect("the fact has a withdrawal"));

    before
        .obligations
        .iter()
        .filter(|obligation| obligation.status == report::Status::Proven)
        .filter(|obligation| {
            after
                .obligations
                .iter()
                .find(|other| other.key == obligation.key)
                .is_none_or(|other| other.status != report::Status::Proven)
        })
        .map(|obligation| obligation.key.to_string())
        .collect()
}

fn verified(workspace: &WorkspaceState) -> report::ProverReport {
    let model = workspace.assemble_model().unwrap();

    assert!(conseqa::analyzer::validate(&model).is_empty());

    let verification = conseqa::analyzer::verification::verify(&model);

    report::obligations(&model, &verification)
}

#[test]
fn a_packet_states_the_delta_the_required_facts_and_what_withdrawing_each_costs() {
    let dir = temp_dir("packet");
    wallet_code(&dir);

    let committed = wallet("read_committed");
    attest_debit(&dir, &committed);

    let serializable = wallet("serializable");
    let derivation = derive(&dir, &serializable);
    let packet = packet_of(&derivation, DEBIT);

    assert_eq!(packet.priority, Some(Priority::Correctness));

    // The delta from the recorded residual.
    let delta = packet.declaration.delta.as_ref().unwrap();

    assert_eq!(delta.len(), 1);
    assert_eq!(delta[0].path, "isolation");
    assert_eq!(delta[0].base, Some(json!("read_committed")));
    assert_eq!(delta[0].head, Some(json!("serializable")));

    assert_eq!(
        packet
            .conditions
            .iter()
            .map(|statement| statement.condition)
            .collect::<Vec<_>>(),
        [
            ConditionKind::Stale,
            ConditionKind::MissingFacts,
            ConditionKind::Surplus
        ]
    );

    // The required facts, with the obligations relying on each.
    let required = required(&derivation.state, DEBIT, ConformanceClass::Attestable);

    assert_eq!(
        packet
            .required
            .iter()
            .map(|section| section.fact.clone())
            .collect::<Vec<_>>(),
        required
    );

    for section in &packet.required {
        assert!(
            section
                .obligations
                .iter()
                .any(|relying| relying.display == DEBIT_SERIALIZABLE),
            "`{}` is relied on by the debit's proof",
            section.rendering
        );

        // The load-bearing check matches an independent verification of
        // the withdrawn model.
        match &section.load_bearing {
            LoadBearing::Withdrawal {
                withdrawal,
                regressions: found,
            } => {
                assert!(!withdrawal.is_empty());
                assert_eq!(
                    found
                        .iter()
                        .map(|regression| regression.display.clone())
                        .collect::<Vec<_>>(),
                    regressions(&serializable, &section.fact),
                    "the withdrawal of `{}`",
                    section.rendering
                );
            }

            other => panic!("`{}` has a withdrawal, not {other:?}", section.rendering),
        }
    }

    // The serializable isolation carries the proof alone: without it
    // the debit is no longer proven serializable.
    let isolation = packet
        .required
        .iter()
        .find(|section| section.fact.kind() == "isolation")
        .unwrap();

    let LoadBearing::Withdrawal { regressions, .. } = &isolation.load_bearing else {
        unreachable!()
    };

    assert!(
        regressions
            .iter()
            .any(|regression| regression.display == DEBIT_SERIALIZABLE
                && regression.before == report::Status::Proven
                && regression.after != Some(report::Status::Proven))
    );

    // The read set names the declaration and the record it read.
    assert!(
        packet
            .read_set
            .declarations
            .iter()
            .any(|read| read.declaration == *key(DEBIT).key())
    );
    assert_eq!(
        packet.read_set.record,
        Some(
            Records::of_project(&dir)
                .read(&key(DEBIT))
                .unwrap()
                .unwrap()
                .digest
        )
    );

    // The rendering states the cost, and embeds no file content.
    let text = packet.render();

    assert!(text.contains("## Required facts"), "{text}");
    assert!(
        text.contains(&format!("{DEBIT_SERIALIZABLE}   proven -> ")),
        "{text}"
    );
    assert!(!text.contains("UPDATE accounts"), "{text}");

    // Its neighbours: the charge's effect site calls its operation.
    let callers = packet
        .code
        .neighbours
        .iter()
        .find(|relation| serde_json::to_value(relation.relation).unwrap() == json!("callers"))
        .unwrap();

    assert_eq!(
        callers
            .listing
            .declarations
            .iter()
            .map(|located| located.declaration.to_string())
            .collect::<Vec<_>>(),
        [CHARGE_SITE]
    );

    // And the guide topics of what it uses.
    for topic in ["Isolation", "Atomic conditional mutations"] {
        assert!(packet.semantics.contains(&topic), "{:?}", packet.semantics);
    }
}

#[test]
fn precedents_come_only_from_current_reviewed_records() {
    let dir = temp_dir("precedents");
    wallet_code(&dir);

    let workspace = wallet("read_committed");
    attest_debit(&dir, &workspace);

    let derivation = derive(&dir, &workspace);

    assert_eq!(kinds(&derivation.state, DEBIT), []);

    let precedents = |packet: &Packet, kind: &str| -> Vec<String> {
        packet
            .precedents
            .iter()
            .find(|precedents| precedents.kind == kind)
            .unwrap_or_else(|| panic!("the packet has {kind} precedents"))
            .precedents
            .iter()
            .map(|precedent| format!("{} {}", precedent.declaration, precedent.rendering))
            .collect()
    };

    // The debit is current and confirmed: the charge's packet cites it.
    let packet = packet_of(&derivation, CHARGE);

    assert_eq!(
        precedents(&packet, "guarded_mutation"),
        [format!(
            "{DEBIT} guarded_mutation(tx.debit, object.account, compare_and_set, atomic, \
             comparing version)"
        )]
    );
    assert_eq!(
        precedents(&packet, "isolation"),
        [format!("{DEBIT} isolation(tx.debit) = read_committed")]
    );

    // Confirmed only by its attester, it is no precedent.
    let records = Records::of_project(&dir);
    let file = records.read(&key(DEBIT)).unwrap().unwrap();
    let mut record = record_of(&dir, DEBIT);

    record.reviews = vec![confirming(&dir, DEBIT, attester())];
    records.write(&record, Some(file.digest)).unwrap();

    let derivation = derive(&dir, &workspace);

    assert_eq!(
        precedents(&packet_of(&derivation, CHARGE), "guarded_mutation"),
        Vec::<String>::new()
    );

    // Nor once its code drifts, however it was reviewed.
    review(&dir, DEBIT, confirming(&dir, DEBIT, reviewer()));
    assert!(!precedents(&packet_of(&derive(&dir, &workspace), CHARGE), "isolation").is_empty());

    let path = dir.join("src/wallet.rs");
    let code = fs::read_to_string(&path).unwrap();

    fs::write(
        &path,
        code.replace("account.balance - amount", "account.balance - amount - 0"),
    )
    .unwrap();

    assert_eq!(
        precedents(&packet_of(&derive(&dir, &workspace), CHARGE), "isolation"),
        Vec::<String>::new()
    );
}

#[test]
fn a_current_declaration_and_an_orphaned_key_have_packets() {
    let dir = temp_dir("orphan");
    wallet_code(&dir);

    put(
        &dir,
        "src/refund.rs",
        "// conseqa:begin transaction operation.refund/tx.refund\n\
         refund();\n\
         // conseqa:end transaction operation.refund/tx.refund\n",
    );

    let workspace = wallet("read_committed");
    attest_debit(&dir, &workspace);

    let derivation = derive(&dir, &workspace);

    // A current declaration has no item, and its packet no conditions.
    let current = packet_of(&derivation, DEBIT);

    assert_eq!(current.priority, None);
    assert_eq!(current.conditions, []);
    assert!(
        current.done_when.iter().all(|step| step.done),
        "{:?}",
        current.done_when
    );

    // An orphaned key's packet says to remove it.
    let orphan = packet_of(&derivation, "transaction operation.refund/tx.refund");

    assert_eq!(orphan.priority, Some(Priority::Cleanup));
    assert_eq!(orphan.declaration.residual, None);
    assert_eq!(
        orphan
            .conditions
            .iter()
            .map(|statement| statement.condition)
            .collect::<Vec<_>>(),
        [ConditionKind::Orphaned]
    );
    assert_eq!(orphan.code.regions.len(), 1);
    assert!(!orphan.done_when[0].done);

    // Any other key has none.
    assert!(packet::packet(&derivation, &key("transaction operation.nope/tx.nope")).is_none());
}

#[test]
fn a_packet_carries_open_disputes_and_the_last_refusal() {
    let dir = temp_dir("disputes");
    wallet_code(&dir);

    let workspace = wallet("read_committed");
    attest_debit(&dir, &workspace);

    // A dispute at the current digests, with its note.
    review(
        &dir,
        DEBIT,
        Review {
            by: party(PartyKind::Person, "ana"),
            digests: record_of(&dir, DEBIT).digests(),
            verdict: Verdict::Confirmed,
            items: vec![ReviewItem {
                target: ReviewTarget::Fact(0),
                verdict: Verdict::Disputed,
                note: Some("the version is compared after the write".to_string()),
            }],
        },
    );

    let packet = packet_of(&derive(&dir, &workspace), DEBIT);

    assert_eq!(packet.disputes.len(), 1);
    assert_eq!(packet.disputes[0].by, party(PartyKind::Person, "ana"));
    assert!(!packet.disputes[0].record);
    assert_eq!(
        packet.disputes[0].items[0].note.as_deref(),
        Some("the version is compared after the write")
    );
    assert!(
        packet
            .render()
            .contains("the version is compared after the write")
    );

    // A refused attest is kept, and its packet says why.
    let charge = state::derive(&dir, &workspace).unwrap();
    let wrong = required(&charge, CHARGE, ConformanceClass::Attestable);

    let refused = refusal(attest_as(
        &dir,
        &workspace,
        &input(&dir, DEBIT, &wrong),
        attester(),
    ));

    assert_eq!(refusal_name(&refused), "not_implementer");

    let last = packet_of(&derive(&dir, &workspace), DEBIT)
        .last_refusal
        .expect("the packet carries the last refusal");

    assert_eq!(last.by, attester());
    assert_eq!(last.refusal["refusal"], json!("not_implementer"));
    assert_eq!(last.message, refused.to_string());

    // It is shown while the record is the one the refused attest saw...
    review(&dir, DEBIT, confirming(&dir, DEBIT, reviewer()));
    assert_eq!(
        packet_of(&derive(&dir, &workspace), DEBIT).last_refusal,
        None
    );

    // ...and an attest that succeeds clears it.
    refusal(attest_as(
        &dir,
        &workspace,
        &input(&dir, DEBIT, &wrong),
        attester(),
    ));
    assert!(
        packet_of(&derive(&dir, &workspace), DEBIT)
            .last_refusal
            .is_some()
    );

    let facts = required(&charge, DEBIT, ConformanceClass::Attestable);

    attest_as(&dir, &workspace, &input(&dir, DEBIT, &facts), attester()).unwrap();

    assert_eq!(
        packet_of(&derive(&dir, &workspace), DEBIT).last_refusal,
        None
    );
}

// ---------------------------------------------------------------------
// §18.5 Attest refusals
// ---------------------------------------------------------------------

/// Every golden fact of a kind.
fn golden(kind: &str) -> Fact {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/facts");

    fs::read_dir(path)
        .unwrap()
        .flat_map(|entry| {
            let golden: Value =
                serde_json::from_str(&fs::read_to_string(entry.unwrap().path()).unwrap()).unwrap();

            golden["facts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|fact| read_canonical::<Fact>(fact).unwrap())
                .collect::<Vec<_>>()
        })
        .find(|fact| fact.kind() == kind)
        .unwrap_or_else(|| panic!("a golden {kind} fact"))
}

#[test]
fn attest_refuses_each_clause_by_name() {
    let dir = temp_dir("refusals");
    wallet_code(&dir);

    let workspace = wallet("read_committed");
    let state = state::derive(&dir, &workspace).unwrap();
    let debit = required(&state, DEBIT, ConformanceClass::Attestable);
    let charge = required(&state, CHARGE, ConformanceClass::Attestable);

    let name = |input: &AttestInput| {
        refusal_name(&refusal(attest_as(&dir, &workspace, input, attester())))
    };

    // A fact no proof relies on, and one required of another
    // implementer.
    let serializable = wallet("serializable");
    let unrelied = required(
        &state::derive(&dir, &serializable).unwrap(),
        DEBIT,
        ConformanceClass::Attestable,
    )
    .into_iter()
    .find(|fact| fact.kind() == "isolation")
    .unwrap();

    assert_eq!(name(&input(&dir, DEBIT, &[unrelied])), "not_required");
    assert_eq!(name(&input(&dir, DEBIT, &charge)), "not_implementer");

    let Refusal::NotImplementer { implementers, .. } = refusal(attest_as(
        &dir,
        &workspace,
        &input(&dir, DEBIT, &charge[..1]),
        attester(),
    )) else {
        unreachable!()
    };

    assert_eq!(texts(&implementers), [CHARGE]);

    // A structural fact is provided by the structural claim.
    assert_eq!(
        name(&input(&dir, DEBIT, &[golden("naturally_replayable")])),
        "not_attestable"
    );

    // Evidence outside every allowed region: here, in the charge's.
    let mut outside = input(&dir, DEBIT, &debit);
    outside.facts[0].evidence = vec![inside(&dir, CHARGE)];

    assert_eq!(name(&outside), "evidence_outside");

    // A marker line is no part of its region.
    let mut marker = input(&dir, DEBIT, &debit);
    let span = inside(&dir, DEBIT);
    marker.facts[0].evidence = vec![Span {
        path: span.path.clone(),
        lines: [span.lines[0] - 1, span.lines[1]],
    }];

    assert_eq!(name(&marker), "evidence_outside");

    // An unbound declaration.
    assert_eq!(
        name(&AttestInput {
            declaration: key(CHARGE_SITE),
            facts: Vec::new(),
            structural: true,
            completeness: Vec::new(),
            read_set: None,
        }),
        "unbound"
    );

    // A key the spec does not declare.
    assert_eq!(
        name(&AttestInput {
            declaration: key("transaction operation.nope/tx.nope"),
            facts: Vec::new(),
            structural: true,
            completeness: Vec::new(),
            read_set: None,
        }),
        "unknown_declaration"
    );

    // A claim that does not hold now, and a pattern that does not
    // compile.
    put(
        &dir,
        "scripts/purge.sql",
        "DELETE FROM accounts WHERE balance = 0;\n",
    );

    let mut claim = input(&dir, ACCOUNT, &[]);
    claim.completeness = vec![writers(&ACCOUNT_WRITERS)];

    let Refusal::ClaimUnheld {
        index,
        matches,
        more,
    } = refusal(attest_as(&dir, &workspace, &claim, attester()))
    else {
        panic!("the claim does not hold");
    };

    assert_eq!((index, more), (0, 0));
    assert_eq!(matches[0].path, "scripts/purge.sql");

    claim.completeness = vec![writers(&["(unclosed"])];

    assert_eq!(name(&claim), "malformed");

    // A fact attested twice.
    let mut twice = input(&dir, DEBIT, &debit);
    twice.facts.push(twice.facts[0].clone());

    assert_eq!(name(&twice), "malformed");

    // A scan error in a file holding the declaration's markers.
    put(
        &dir,
        "src/broken.rs",
        "// conseqa:begin transaction operation.debit/tx.debit\n\
         broken();\n",
    );

    let Refusal::ScanError { errors } = refusal(attest_as(
        &dir,
        &workspace,
        &input(&dir, DEBIT, &debit),
        attester(),
    )) else {
        panic!("the scan error refuses the attest");
    };

    assert_eq!(errors[0].path, "src/broken.rs");

    fs::remove_file(dir.join("src/broken.rs")).unwrap();

    // A stale read set: the packet was built at read committed.
    let packet = packet_of(&derive(&dir, &workspace), DEBIT);

    let mut stale = input(&dir, DEBIT, &serializable_facts(&dir, &serializable));
    stale.read_set = Some(packet.read_set.clone());

    let Refusal::Stale { changes } = refusal(attest_as(&dir, &serializable, &stale, attester()))
    else {
        panic!("the read set is stale");
    };

    assert!(changes.contains(&ReadSetChange::Changed {
        declaration: key(DEBIT).into_key()
    }));

    // A replaced record that changed since the packet read it.
    let mut replaced = input(&dir, DEBIT, &debit);
    replaced.read_set = Some(packet.read_set.clone());

    attest_as(
        &dir,
        &workspace,
        &input(&dir, DEBIT, &debit[..1]),
        attester(),
    )
    .unwrap();

    let Refusal::RecordChanged { expected, found } =
        refusal(attest_as(&dir, &workspace, &replaced, attester()))
    else {
        panic!("the record changed");
    };

    assert_eq!(expected, None);
    assert!(found.is_some());
}

#[test]
fn evidence_may_lie_in_a_region_of_one_of_the_facts_anchors() {
    let dir = temp_dir("anchored");
    wallet_code(&dir);

    let workspace = wallet("read_committed");
    let state = state::derive(&dir, &workspace).unwrap();
    let debit = required(&state, DEBIT, ConformanceClass::Attestable);

    // The guarded mutations compare the account's version column, so
    // they anchor the account: its table's region may evidence them.
    // The isolation anchors only the transaction.
    let anchored = |kind: &str| {
        let mut input = input(&dir, DEBIT, &debit);

        for attested in &mut input.facts {
            if attested.fact.kind() == kind {
                attested.evidence = vec![inside(&dir, ACCOUNT)];
            }
        }

        input
    };

    assert_eq!(
        refusal_name(&refusal(attest_as(
            &dir,
            &workspace,
            &anchored("isolation"),
            attester()
        ))),
        "evidence_outside"
    );

    assert!(
        attest_as(&dir, &workspace, &anchored("guarded_mutation"), attester())
            .unwrap()
            .written
    );
}

/// The facts the debit needs at serializable isolation.
fn serializable_facts(dir: &Path, serializable: &WorkspaceState) -> Vec<Fact> {
    required(
        &state::derive(dir, serializable).unwrap(),
        DEBIT,
        ConformanceClass::Attestable,
    )
}

// ---------------------------------------------------------------------
// §14.1, §14.2 What attest writes
// ---------------------------------------------------------------------

#[test]
fn attest_computes_the_record_and_replays_as_a_no_op() {
    let dir = temp_dir("writes");
    wallet_code(&dir);

    let workspace = wallet("read_committed");
    let state = state::derive(&dir, &workspace).unwrap();
    let facts = required(&state, DEBIT, ConformanceClass::Attestable);

    // Facts in any order, spans repeated: the record holds them in
    // canonical order, each span once.
    let mut reversed: Vec<Fact> = facts.clone();
    reversed.reverse();

    let mut first = input(&dir, DEBIT, &reversed);
    let span = first.facts[0].evidence[0].clone();
    first.facts[0].evidence.push(span.clone());
    first.structural = true;
    first.read_set = Some(packet_of(&derive(&dir, &workspace), DEBIT).read_set);

    let attested = attest_as(&dir, &workspace, &first, attester()).unwrap();

    assert!(attested.written);
    assert_eq!(
        attested.record,
        ".conseqa/conformance/transaction/operation.debit/tx.debit.yaml"
    );

    let record = record_of(&dir, DEBIT);

    assert_eq!(
        record
            .facts
            .iter()
            .map(|attested| attested.fact.clone())
            .collect::<Vec<_>>(),
        {
            let mut sorted = facts.clone();
            sorted.sort_by_key(Fact::canonical);
            sorted
        }
    );
    assert!(
        record
            .facts
            .iter()
            .all(|attested| attested.evidence == [span.clone()])
    );
    assert_eq!(record.attested.by, attester());
    assert_eq!(record.attested.commit, None);
    assert!(record.structural);

    // The tool computed the regions and the residual.
    let scan = markers::scan(&dir).unwrap();

    assert_eq!(
        record.regions,
        scan.regions_of(key(DEBIT).key())
            .map(RecordedRegion::from)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        record.residual,
        Partition::of(&workspace).members[key(DEBIT).key()]
    );

    // Replaying the same attest writes nothing, though its read set
    // names the record it replaced as absent.
    let replay = attest_as(&dir, &workspace, &first, attester()).unwrap();

    assert!(!replay.written);
    assert_eq!(replay.digest, attested.digest);

    // A review survives an attest of exactly the same record...
    review(&dir, DEBIT, confirming(&dir, DEBIT, reviewer()));

    let mut again = first.clone();
    again.read_set = None;

    let kept = attest_as(&dir, &workspace, &again, party(PartyKind::Agent, "another")).unwrap();

    assert!(kept.written);
    assert_eq!(kept.dropped_reviews, 0);
    assert_eq!(record_of(&dir, DEBIT).reviews.len(), 1);

    // ...and not one attesting anything else. Partial attestation
    // leaves the rest missing.
    let partial = attest_as(
        &dir,
        &workspace,
        &input(&dir, DEBIT, &facts[..1]),
        attester(),
    )
    .unwrap();

    assert_eq!(partial.dropped_reviews, 1);
    assert_eq!(record_of(&dir, DEBIT).reviews, []);
    assert!(
        kinds(&state::derive(&dir, &workspace).unwrap(), DEBIT)
            .contains(&ConditionKind::MissingFacts)
    );
}

#[test]
fn an_attest_never_names_who_attests() {
    // The surface stamps `by` from the credential the call came with.
    let input = json!({
        "declaration": DEBIT,
        "facts": [],
        "structural": true,
        "by": { "kind": "agent", "name": "self-declared" },
    });

    assert!(serde_json::from_value::<AttestInput>(input).is_err());

    let input = json!({
        "declaration": DEBIT,
        "structural": true,
    });

    assert!(serde_json::from_value::<AttestInput>(input).is_ok());
}

#[test]
fn a_packets_read_set_round_trips_into_an_attest() {
    let dir = temp_dir("round-trip");
    wallet_code(&dir);

    let workspace = wallet("read_committed");
    let derivation = derive(&dir, &workspace);
    let packet = packet_of(&derivation, DEBIT);
    let facts = required(&derivation.state, DEBIT, ConformanceClass::Attestable);

    let mut sent = serde_json::to_value(input(&dir, DEBIT, &facts)).unwrap();
    sent["read_set"] = serde_json::to_value(&packet.read_set).unwrap();

    let input: AttestInput = serde_json::from_value(sent).unwrap();

    assert_eq!(input.read_set.as_ref(), Some(&packet.read_set));
    assert!(
        attest_as(&dir, &workspace, &input, attester())
            .unwrap()
            .written
    );
}

// ---------------------------------------------------------------------
// §14.3, §18.9 Reviews
// ---------------------------------------------------------------------

fn review_of(text: &str, verdict: Verdict, items: Vec<ReviewItem>) -> ReviewInput {
    ReviewInput {
        declaration: key(text),
        verdict,
        items,
        record: None,
    }
}

fn record_review(
    dir: &Path,
    workspace: &WorkspaceState,
    input: &ReviewInput,
    by: Party,
) -> Result<Reviewed, ReviewError> {
    reviews::record_review(dir, workspace, input, by)
}

fn review_refusal(result: Result<Reviewed, ReviewError>) -> String {
    match result {
        Err(ReviewError::Refused(refusal)) => serde_json::to_value(&*refusal).unwrap()["refusal"]
            .as_str()
            .unwrap()
            .to_string(),
        Err(ReviewError::Failed(error)) => panic!("the review failed: {error}"),
        Ok(reviewed) => panic!("the review was not refused: {reviewed:?}"),
    }
}

/// Attests the debit's required facts, unreviewed.
fn attest_debit_alone(dir: &Path, workspace: &WorkspaceState) {
    let state = state::derive(dir, workspace).unwrap();
    let facts = required(&state, DEBIT, ConformanceClass::Attestable);

    attest_as(dir, workspace, &input(dir, DEBIT, &facts), attester()).unwrap();
}

fn edit_the_debit(dir: &Path) {
    let path = dir.join("src/wallet.rs");
    let code = fs::read_to_string(&path).unwrap();

    fs::write(
        &path,
        code.replace("account.balance - amount", "account.balance - amount - 0"),
    )
    .unwrap();
}

#[test]
fn a_review_confirms_only_from_another_party_and_lapses_with_the_code() {
    let dir = temp_dir("reviews");
    wallet_code(&dir);

    let workspace = wallet("read_committed");
    attest_debit_alone(&dir, &workspace);

    let unreviewed = || kinds(&state::derive(&dir, &workspace).unwrap(), DEBIT);

    assert!(unreviewed().contains(&ConditionKind::Unreviewed));

    // The attester reviewing its own record satisfies nothing, from
    // another job as from its own.
    let mut itself = attester();
    itself.job = Some("job-2".to_string());

    let recorded = record_review(
        &dir,
        &workspace,
        &review_of(DEBIT, Verdict::Confirmed, Vec::new()),
        itself.clone(),
    )
    .unwrap();

    assert!(recorded.written);
    assert_eq!(record_of(&dir, DEBIT).reviews[0].by, itself);
    assert!(unreviewed().contains(&ConditionKind::Unreviewed));

    // Another party's confirmation does, at the digests the tool read.
    record_review(
        &dir,
        &workspace,
        &review_of(DEBIT, Verdict::Confirmed, Vec::new()),
        reviewer(),
    )
    .unwrap();

    let record = record_of(&dir, DEBIT);

    assert_eq!(record.reviews.len(), 2);
    assert_eq!(record.reviews[1].digests, record.digests());
    assert!(!unreviewed().contains(&ConditionKind::Unreviewed));

    // An edit lapses it: the record drifts from the code the review saw,
    // and the drifted record cannot be reviewed until attested again.
    edit_the_debit(&dir);

    assert!(unreviewed().contains(&ConditionKind::Drifted));
    assert_eq!(
        review_refusal(record_review(
            &dir,
            &workspace,
            &review_of(DEBIT, Verdict::Confirmed, Vec::new()),
            reviewer(),
        )),
        "drifted"
    );

    // Attested again, the record drops the review of other code.
    attest_debit_alone(&dir, &workspace);

    assert_eq!(record_of(&dir, DEBIT).reviews, []);
    assert!(unreviewed().contains(&ConditionKind::Unreviewed));
}

#[test]
fn a_disputed_review_raises_disputed_whoever_gives_it() {
    let dir = temp_dir("disputed");
    wallet_code(&dir);

    let workspace = wallet("read_committed");
    attest_debit_alone(&dir, &workspace);

    // The attester's own dispute counts.
    record_review(
        &dir,
        &workspace,
        &review_of(
            DEBIT,
            Verdict::Confirmed,
            vec![ReviewItem {
                target: ReviewTarget::Fact(1),
                verdict: Verdict::Disputed,
                note: Some("the version is compared after the write".to_string()),
            }],
        ),
        attester(),
    )
    .unwrap();

    assert!(
        kinds(&state::derive(&dir, &workspace).unwrap(), DEBIT).contains(&ConditionKind::Disputed)
    );
}

#[test]
fn a_review_is_refused_by_name_when_the_record_cannot_be_reviewed() {
    let dir = temp_dir("review-refusals");
    wallet_code(&dir);

    let workspace = wallet("read_committed");

    let refused = |text: &str| {
        review_refusal(record_review(
            &dir,
            &workspace,
            &review_of(text, Verdict::Confirmed, Vec::new()),
            reviewer(),
        ))
    };

    assert_eq!(
        refused("transaction operation.nope/tx.nope"),
        "unknown_declaration"
    );
    assert_eq!(refused(DEBIT), "no_record");

    attest_debit_alone(&dir, &workspace);

    // An item naming a fact the record does not hold.
    assert_eq!(
        review_refusal(record_review(
            &dir,
            &workspace,
            &review_of(
                DEBIT,
                Verdict::Confirmed,
                vec![ReviewItem {
                    target: ReviewTarget::Fact(9),
                    verdict: Verdict::Confirmed,
                    note: None,
                }],
            ),
            reviewer(),
        )),
        "malformed"
    );

    // A record that changed since the reviewer read it.
    let mut stale_read = review_of(DEBIT, Verdict::Confirmed, Vec::new());
    stale_read.record = Some(conseqa::conformance::Digest::of(b"another record"));

    let Err(ReviewError::Refused(refusal)) =
        record_review(&dir, &workspace, &stale_read, reviewer())
    else {
        panic!("the record changed since it was read");
    };

    assert!(matches!(*refusal, ReviewRefusal::RecordChanged { .. }));

    // A record the spec has moved past.
    assert_eq!(
        review_refusal(record_review(
            &dir,
            &wallet("serializable"),
            &review_of(DEBIT, Verdict::Confirmed, Vec::new()),
            reviewer(),
        )),
        "stale"
    );
}

#[test]
fn a_party_keeps_one_review_per_digests_and_a_replay_writes_nothing() {
    let dir = temp_dir("review-replay");
    wallet_code(&dir);

    let workspace = wallet("read_committed");
    attest_debit_alone(&dir, &workspace);

    let read = Records::of_project(&dir)
        .read(&key(DEBIT))
        .unwrap()
        .unwrap()
        .digest;

    let mut confirmed = review_of(DEBIT, Verdict::Confirmed, Vec::new());
    confirmed.record = Some(read);

    let first = record_review(&dir, &workspace, &confirmed, reviewer()).unwrap();

    assert!(first.written);
    assert!(!first.replaced);

    // Replayed, though the record it names is gone: nothing to write.
    let replay = record_review(&dir, &workspace, &confirmed, reviewer()).unwrap();

    assert!(!replay.written);
    assert_eq!(replay.digest, first.digest);

    // A new verdict by the same party replaces its review, from any job.
    let mut later = reviewer();
    later.job = Some("job-9".to_string());

    let disputed = record_review(
        &dir,
        &workspace,
        &review_of(DEBIT, Verdict::Disputed, Vec::new()),
        later.clone(),
    )
    .unwrap();

    assert!(disputed.written);
    assert!(disputed.replaced);

    let record = record_of(&dir, DEBIT);

    assert_eq!(record.reviews.len(), 1);
    assert_eq!(record.reviews[0].by, later);
    assert_eq!(record.reviews[0].verdict, Verdict::Disputed);
}

// ---------------------------------------------------------------------
// §15.3 Gates
// ---------------------------------------------------------------------

#[test]
fn a_check_fails_on_scan_errors_and_on_the_gates_named_within_its_scope() {
    let dir = temp_dir("gates");
    wallet_code(&dir);

    let committed = wallet("read_committed");
    attest_debit(&dir, &committed);

    let derivation = derive(&dir, &committed);

    // The charge owes its facts. Each closure holds the other's
    // transaction, so that blocks both proofs, though the debit itself
    // is current. With no gate, nothing fails.
    assert!(check::check(&derivation, None, &[]).passed);

    let failing = check::check(
        &derivation,
        None,
        &[
            Gate::NotConformant,
            Gate::Condition(ConditionKind::MissingFacts),
            Gate::Condition(ConditionKind::Disputed),
        ],
    );

    assert!(!failing.passed);
    assert_eq!(
        failing
            .failed
            .iter()
            .map(|failure| failure.gate)
            .collect::<Vec<_>>(),
        [
            Gate::NotConformant,
            Gate::Condition(ConditionKind::MissingFacts)
        ]
    );
    assert_eq!(
        texts(&failing.failed[0].obligations),
        [CHARGE_SERIALIZABLE, DEBIT_SERIALIZABLE]
    );
    assert_eq!(texts(&failing.failed[1].items), [CHARGE]);

    // Scoped to a change of the debit's isolation, what the change owes
    // is in scope, the charge included, since the debit's moved proof
    // rests on the charge's guarded mutations too. The programs' missing
    // markers are no part of it.
    let serializable = wallet("serializable");
    let head = derive(&dir, &serializable);

    let scope = Scope::between(
        &side(SideName::Base, committed.clone()),
        &side(SideName::Head, serializable.clone()),
    );

    let unbound = [Gate::Condition(ConditionKind::Unbound)];

    assert!(!check::check(&head, None, &unbound).passed);
    assert!(check::check(&head, Some(&scope), &unbound).passed);

    let scoped = check::check(
        &head,
        Some(&scope),
        &[Gate::Condition(ConditionKind::MissingFacts)],
    );

    assert_eq!(texts(&scoped.failed[0].items), [CHARGE, DEBIT]);

    // A scan error fails every check.
    put(
        &dir,
        "src/broken.rs",
        "// conseqa:begin transaction operation.debit/tx.debit\n",
    );

    let broken = check::check(&derive(&dir, &committed), None, &[]);

    assert!(!broken.passed);
    assert_eq!(broken.scan_errors.len(), 1);

    // Gates are named as conditions are.
    assert_eq!(
        "missing_facts".parse::<Gate>(),
        Ok(Gate::Condition(ConditionKind::MissingFacts))
    );
    assert_eq!("not_conformant".parse::<Gate>(), Ok(Gate::NotConformant));
    assert!("conformant".parse::<Gate>().is_err());
}

// ---------------------------------------------------------------------
// T1, §18.10 Documents
// ---------------------------------------------------------------------

#[test]
fn documents_name_their_format_and_kind_and_are_byte_identical() {
    let dir = temp_dir("documents");
    wallet_code(&dir);

    let workspace = wallet("read_committed");
    attest_debit(&dir, &workspace);

    let render = || {
        let derivation = derive(&dir, &workspace);
        let work = plan::plan(&derivation);

        [
            document::status(&derivation.state).to_json(),
            document::plan(&work.items, None).to_json(),
            document::item(&packet_of(&derivation, CHARGE)).to_json(),
            document::check(&check::check(&derivation, None, &[]), None).to_json(),
        ]
    };

    let first = render();

    assert_eq!(first, render());

    for (json, kind) in first.iter().zip(["status", "plan", "item", "check"]) {
        let document: Value = serde_json::from_str(json).unwrap();

        assert_eq!(document["format"], json!(document::FORMAT), "{kind}");
        assert_eq!(document["document"], json!(kind));
        assert_eq!(document["dsl"], json!(6));
        assert_eq!(document["fact_format"], json!(1));
    }

    // An attest's outcome, written or refused, by name.
    let state = state::derive(&dir, &workspace).unwrap();
    let facts = required(&state, CHARGE, ConformanceClass::Attestable);
    let attested = attest_as(&dir, &workspace, &input(&dir, CHARGE, &facts), attester()).unwrap();

    let written: Value = serde_json::from_str(
        &document::attest(document::AttestOutcome::of(Ok(&attested))).to_json(),
    )
    .unwrap();

    assert_eq!(written["document"], json!("attest"));
    assert_eq!(written["outcome"], json!("written"));
    assert_eq!(written["record"], json!(attested.record));

    let refused = refusal(attest_as(
        &dir,
        &workspace,
        &input(&dir, DEBIT, &facts),
        attester(),
    ));

    let refused: Value = serde_json::from_str(
        &document::attest(document::AttestOutcome::of(Err(&refused))).to_json(),
    )
    .unwrap();

    assert_eq!(refused["outcome"], json!("refused"));
    assert_eq!(refused["refusal"], json!("not_implementer"));
    assert!(refused["message"].as_str().unwrap().contains("required of"));
}

// ---------------------------------------------------------------------
// §14.5 Provenance
// ---------------------------------------------------------------------

#[test]
fn the_person_running_a_checkout_is_its_git_user_name() {
    let dir = temp_dir("person");

    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(args)
            .status()
            .unwrap();

        assert!(status.success(), "git {args:?}");
    };

    git(&["init", "-q"]);
    git(&["config", "user.name", "Ana Tester"]);

    assert_eq!(
        Party::person(&dir).unwrap(),
        party(PartyKind::Person, "Ana Tester")
    );

    // The rule that a reviewer is another party ignores jobs.
    let mut first = party(PartyKind::Agent, "claude/claude-opus-5-5");
    let mut second = first.clone();

    first.job = Some("job-1".to_string());
    second.job = Some("job-2".to_string());

    assert!(first.same_as(&second));
    assert!(!first.same_as(&party(PartyKind::Agent, "codex")));

    let _ = ConformanceError::NoPerson { path: dir };
}
