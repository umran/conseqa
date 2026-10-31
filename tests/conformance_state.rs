//! Required facts, completeness claims, and conformance state (§7, §9,
//! and §10 of the conformance and implementation guidance revision; its
//! tests §18.4 and §18.6).

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use serde_json::{Value, json};

use conseqa::{
    analyzer::{
        basis::{ConformanceClass, DeclarationKey, Fact, Implementers, TextualKey, read_canonical},
        report,
    },
    conformance::{
        ConformanceError,
        completeness::{self, Match, Matcher},
        markers,
        record::{
            Attestation, AttestedFact, ClaimScope, CompletenessClaim, Exception, Party, PartyKind,
            Record, RecordedRegion, Records, Review, ReviewItem, ReviewTarget, Span, Verdict,
        },
        state::{
            self, Blocker, Condition, ConditionKind, DeclarationState, ObligationConformance,
            ScopeFailure, State,
        },
    },
    design::{
        ProjectDirectory, ProjectStore, RunId, RunMetadata, WorkspaceState,
        change::partition::Partition, graph_build, load_source,
    },
    parser::yaml,
    spec::Id,
};

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "conseqa-conformance-state-{label}-{}",
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

fn fixture(name: &str) -> WorkspaceState {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);

    spec(&fs::read_to_string(path).unwrap())
}

fn key(text: &str) -> TextualKey {
    text.parse().unwrap()
}

fn party(kind: PartyKind, name: &str) -> Party {
    Party {
        kind,
        name: name.to_string(),
    }
}

fn attester() -> Party {
    party(PartyKind::Agent, "attester")
}

fn reviewer() -> Party {
    party(PartyKind::Agent, "reviewer")
}

fn derive(dir: &Path, workspace: &WorkspaceState) -> State {
    state::derive(dir, workspace).unwrap()
}

fn declaration<'a>(state: &'a State, text: &str) -> &'a DeclarationState {
    state
        .declaration(&key(text))
        .unwrap_or_else(|| panic!("`{text}` is a bindable declaration of the spec"))
}

fn kinds(state: &State, text: &str) -> Vec<ConditionKind> {
    declaration(state, text)
        .conditions
        .iter()
        .map(Condition::kind)
        .collect()
}

fn condition<'a>(state: &'a State, text: &str, kind: ConditionKind) -> &'a Condition {
    declaration(state, text)
        .conditions
        .iter()
        .find(|condition| condition.kind() == kind)
        .unwrap_or_else(|| panic!("`{text}` is {kind}"))
}

/// A proven obligation's conformance, by its key as displayed.
fn obligation<'a>(state: &'a State, display: &str) -> &'a ObligationConformance {
    state
        .obligations
        .iter()
        .find(|conformance| conformance.obligation.to_string() == display)
        .unwrap_or_else(|| panic!("`{display}` is a proven obligation"))
}

/// The facts of one class required of a declaration.
fn required(state: &State, text: &str, class: ConformanceClass) -> Vec<Fact> {
    declaration(state, text)
        .required
        .iter()
        .filter(|required| required.class == class)
        .map(|required| required.fact.clone())
        .collect()
}

fn displayed(facts: &[Fact]) -> Vec<String> {
    facts.iter().map(ToString::to_string).collect()
}

fn writers(patterns: &[&str]) -> CompletenessClaim {
    CompletenessClaim {
        scope: ClaimScope::Writers,
        patterns: patterns.iter().map(|pattern| pattern.to_string()).collect(),
        exceptions: Vec::new(),
    }
}

/// What a test attests of one declaration.
struct Attest {
    facts: Vec<Fact>,
    structural: bool,
    completeness: Vec<CompletenessClaim>,
    reviews: Vec<Party>,
}

impl Attest {
    fn facts(facts: Vec<Fact>) -> Self {
        Self {
            facts,
            structural: false,
            completeness: Vec::new(),
            reviews: Vec::new(),
        }
    }

    fn structural(mut self) -> Self {
        self.structural = true;
        self
    }

    fn claims(mut self, completeness: Vec<CompletenessClaim>) -> Self {
        self.completeness = completeness;
        self
    }

    fn reviewed_by(mut self, by: Party) -> Self {
        self.reviews.push(by);
        self
    }
}

/// Writes a declaration's record as attest does: the regions bound to
/// it now and its current residual, each fact evidenced inside its
/// first region, and each review confirming the record at its digests.
/// It replaces whatever record is there.
fn attest(dir: &Path, workspace: &WorkspaceState, declaration: &str, attest: Attest) -> Record {
    let key = key(declaration);
    let scan = markers::scan(dir).unwrap();

    let regions: Vec<RecordedRegion> = scan
        .regions_of(key.key())
        .map(RecordedRegion::from)
        .collect();

    let first = regions.first().expect("an attested declaration is bound");
    let span = Span {
        path: first.path.clone(),
        lines: [first.lines[0] + 1, first.lines[1] - 1],
    };

    let mut record = Record::new(
        key.clone(),
        Partition::of(workspace).members[key.key()].clone(),
        Attestation {
            by: attester(),
            commit: None,
        },
    );

    record.regions = regions;
    record.structural = attest.structural;
    record.completeness = attest.completeness;
    record.facts = attest
        .facts
        .into_iter()
        .map(|fact| AttestedFact {
            fact,
            evidence: vec![span.clone()],
        })
        .collect();

    record.reviews = attest
        .reviews
        .into_iter()
        .map(|by| Review {
            by,
            digests: record.digests(),
            verdict: Verdict::Confirmed,
            items: Vec::new(),
        })
        .collect();

    write(dir, &record);

    record
}

/// Writes a record over whatever is there.
fn write(dir: &Path, record: &Record) {
    let records = Records::of_project(dir);
    let replaces = records
        .read(&record.declaration)
        .unwrap()
        .map(|file| file.digest);

    records.write(record, replaces).unwrap();
}

// ---------------------------------------------------------------------
// §7.1, §7.2 Conformance classes and implementers
// ---------------------------------------------------------------------

/// Every golden fact of every kind, in its durable form.
fn golden_facts() -> Vec<Fact> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/facts");
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();

    paths.sort();

    paths
        .iter()
        .flat_map(|path| {
            let golden: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();

            golden["facts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|fact| read_canonical(fact).unwrap())
                .collect::<Vec<_>>()
        })
        .collect()
}

fn kind_of(fact: &Fact) -> String {
    serde_json::to_value(fact).unwrap()["kind"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn every_fact_kind_has_its_class_and_implementer_rule() {
    let facts = golden_facts();

    let kinds: BTreeSet<String> = facts.iter().map(kind_of).collect();
    assert_eq!(kinds.len(), 25, "a golden sample of every fact kind");

    for fact in &facts {
        let kind = kind_of(fact);

        let class = match kind.as_str() {
            "isolation" | "lock" | "guarded_mutation" | "cursor" | "fence" | "deduplicated_by"
            | "delivery" | "message_identity" | "outbox_redrive" | "retry_may_repeat"
            | "external_guarantee" | "propagation" | "realization" => ConformanceClass::Attestable,

            "naturally_replayable"
            | "artifact_route"
            | "admits_no_messages"
            | "no_admitted_paths"
            | "returns_no_result"
            | "terminal"
            | "stable_root" => ConformanceClass::Structural,

            "conflict_closure" | "cyclic_components" | "no_deleters" => ConformanceClass::ModelWide,

            "relies_on" | "keyed_requirement" => ConformanceClass::Internal,

            other => panic!("`{other}` is not a fact kind of Part I §10.9"),
        };

        assert_eq!(fact.conformance_class(), class, "{kind}");

        let rule = fact.implementers();

        assert_eq!(
            rule.is_some(),
            class == ConformanceClass::Attestable,
            "{kind}"
        );

        match (kind.as_str(), rule) {
            ("isolation" | "lock" | "guarded_mutation" | "deduplicated_by", Some(rule)) => {
                assert!(
                    matches!(
                        rule,
                        Implementers::Named(DeclarationKey::Transaction { .. })
                    ),
                    "{kind}"
                );
            }

            ("delivery", Some(rule)) => assert!(matches!(
                rule,
                Implementers::Named(DeclarationKey::SubscriptionRuntime { .. })
            )),

            ("outbox_redrive", Some(rule)) => {
                assert!(matches!(
                    rule,
                    Implementers::Named(DeclarationKey::Outbox { .. })
                ))
            }

            ("external_guarantee", Some(rule)) => assert!(matches!(
                rule,
                Implementers::Named(DeclarationKey::EffectSite { .. })
            )),

            // The caller's or producer's effect site, or the transition
            // whose side effect it is.
            ("retry_may_repeat" | "propagation", Some(rule)) => assert!(
                matches!(
                    rule,
                    Implementers::Named(
                        DeclarationKey::EffectSite { .. } | DeclarationKey::Transition { .. }
                    )
                ),
                "{kind}"
            ),

            ("realization", Some(Implementers::Named(declaration))) => {
                assert_eq!(
                    declaration.to_string(),
                    "transaction operation.apply_payment/tx.apply_payment"
                );
            }

            ("cursor", Some(rule)) => {
                assert!(matches!(rule, Implementers::CursorAdvancers { .. }))
            }

            ("fence", Some(rule)) => assert!(matches!(rule, Implementers::Fencers { .. })),

            ("message_identity", Some(rule)) => {
                assert!(matches!(rule, Implementers::Producers { .. }))
            }

            (_, None) => {}

            (kind, Some(rule)) => panic!("`{kind}` has an unexpected rule: {rule:?}"),
        }
    }

    // Both kinds of site, both kinds of owner.
    let owners: BTreeSet<&str> = facts
        .iter()
        .filter(|fact| kind_of(fact) == "retry_may_repeat")
        .map(|fact| match fact.implementers() {
            Some(Implementers::Named(declaration)) => declaration.kind(),
            _ => unreachable!(),
        })
        .collect();

    assert_eq!(owners, BTreeSet::from(["effect_site", "transition"]));
}

#[test]
fn implementers_are_resolved_against_the_model() {
    // The cursor's implementers: every transaction advancing it.
    let ledger = fixture("tenant_ledger.yaml");
    let graph = graph_build::build(&ledger, None);

    let cursor = Fact::Cursor {
        data_model: Id("data.ledger".to_string()),
        object: Id("object.tenant_ledger".to_string()),
        field: serde_json::from_value(json!(["last_applied_sequence"])).unwrap(),
        rule: serde_json::from_value(json!("successor")).unwrap(),
    };

    assert_eq!(
        displayed_keys(state::implementers(&cursor, &ledger, &graph)),
        ["transaction operation.apply_entry/tx.apply_entry"]
    );

    // A message identity's: every producer of the schema on the
    // channel, a transition's side effect included, and an outbox's
    // writers.
    assert_eq!(
        producers("flash_checkout.yaml"),
        ["transition machine.order_lifecycle/transition.order.mark_paid"]
    );

    assert_eq!(
        producers("payment_capture.yaml"),
        [
            "effect_site operation.capture_payment/effect.capture_payment.outbox_captured",
            "effect_site operation.publish_payment_event/effect.publish_payment_event.captured",
        ]
    );

    // A fact of another class has none.
    let shop = fixture("shop.yaml");
    let graph = graph_build::build(&shop, None);

    let no_deleters = Fact::NoDeleters {
        data_model: Id("data.shop".to_string()),
        object: Id("object.order".to_string()),
    };

    assert!(state::implementers(&no_deleters, &shop, &graph).is_empty());
}

/// The implementers of every message identity a fixture's proofs rely
/// on.
fn producers(name: &str) -> Vec<String> {
    let workspace = fixture(name);
    let graph = graph_build::build(&workspace, None);

    let producers: BTreeSet<String> = verified(&workspace)
        .obligations
        .iter()
        .flat_map(|obligation| obligation.basis.iter().flatten())
        .filter(|basis_fact| kind_of(&basis_fact.fact) == "message_identity")
        .flat_map(|basis_fact| {
            displayed_keys(state::implementers(&basis_fact.fact, &workspace, &graph))
        })
        .collect();

    producers.into_iter().collect()
}

fn displayed_keys(keys: BTreeSet<DeclarationKey>) -> Vec<String> {
    keys.iter().map(ToString::to_string).collect()
}

fn verified(workspace: &WorkspaceState) -> report::ProverReport {
    let model = workspace.assemble_model().unwrap();
    let verification = conseqa::analyzer::verification::verify(&model);

    report::obligations(&model, &verification)
}

// ---------------------------------------------------------------------
// §18.4 Required facts and obligation conformance
// ---------------------------------------------------------------------

/// One account debited under an observed-version `compare_and_set`: its
/// serializability proof rests on the guarded mutation at
/// `read_committed`, and on serializable isolation alone at
/// `serializable`.
const WALLET: &str = r#"
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
  operation.debit:
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

const DEBIT: &str = "transaction operation.debit/tx.debit";
const ACCOUNT: &str = "data_object data.wallet/object.account";
const DEBIT_SERIALIZABLE: &str = "SerializableBy(input.debit.request.account_id) on tx.debit";

fn wallet(isolation: &str) -> WorkspaceState {
    spec(&WALLET.replace("ISOLATION", isolation))
}

/// The wallet's code: the debit, and the accounts table.
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
         // conseqa:end transaction operation.debit/tx.debit\n",
    );

    put(
        dir,
        "migrations/0001_accounts.sql",
        "-- conseqa:begin data_object data.wallet/object.account\n\
         CREATE TABLE accounts (account_id uuid PRIMARY KEY, balance bigint, version bigint);\n\
         -- conseqa:end data_object data.wallet/object.account\n",
    );
}

const ACCOUNT_WRITERS: [&str; 1] = [r"(?i)\b(update|delete\s+from|insert\s+into)\s+accounts\b"];

#[test]
fn an_attested_guarded_mutation_is_conformant_and_serializable_isolation_makes_it_stale() {
    let dir = temp_dir("wallet");
    wallet_code(&dir);

    let committed = wallet("read_committed");
    let before = derive(&dir, &committed);

    // The proof rests on the guarded mutation, observed version
    // included, and on the transaction's isolation; its closure needs
    // the account's writers claimed.
    assert_eq!(
        displayed(&required(&before, DEBIT, ConformanceClass::Attestable)),
        [
            "guarded_mutation(tx.debit, object.account, compare_and_set, atomic, comparing version)",
            "guarded_mutation(tx.debit, object.account, compare_and_set, \
             observed_version(read.debit.account, version), comparing version)",
            "isolation(tx.debit) = read_committed",
        ]
    );

    assert_eq!(
        declaration(&before, ACCOUNT)
            .claims
            .iter()
            .map(|scope| scope.scope)
            .collect::<Vec<_>>(),
        [ClaimScope::Writers]
    );

    assert_eq!(
        kinds(&before, DEBIT),
        [ConditionKind::Unattested, ConditionKind::MissingFacts]
    );
    assert!(!obligation(&before, DEBIT_SERIALIZABLE).conformant);

    // 1. Bind and attest: the transaction is current, and the
    //    obligation conformant.
    attest(
        &dir,
        &committed,
        DEBIT,
        Attest::facts(required(&before, DEBIT, ConformanceClass::Attestable))
            .reviewed_by(reviewer()),
    );

    attest(
        &dir,
        &committed,
        ACCOUNT,
        Attest::facts(Vec::new())
            .claims(vec![writers(&ACCOUNT_WRITERS)])
            .reviewed_by(reviewer()),
    );

    let attested = derive(&dir, &committed);

    assert_eq!(kinds(&attested, DEBIT), []);
    assert_eq!(kinds(&attested, ACCOUNT), []);

    let conformance = obligation(&attested, DEBIT_SERIALIZABLE);

    assert!(conformance.conformant);
    assert_eq!(conformance.blockers, []);

    // 2. Serializable isolation: the record is stale, the isolation it
    //    now needs is missing, and what it attested is no longer relied
    //    on. The obligation stays proven, and is not conformant.
    let serializable = wallet("serializable");
    let changed = derive(&dir, &serializable);

    assert_eq!(
        kinds(&changed, DEBIT),
        [
            ConditionKind::Stale,
            ConditionKind::MissingFacts,
            ConditionKind::Surplus,
        ]
    );

    let Condition::MissingFacts { facts, structural } =
        condition(&changed, DEBIT, ConditionKind::MissingFacts)
    else {
        unreachable!()
    };

    assert_eq!(displayed(facts), ["isolation(tx.debit) = serializable"]);
    assert_eq!(structural, &[]);

    let Condition::Surplus { facts } = condition(&changed, DEBIT, ConditionKind::Surplus) else {
        unreachable!()
    };

    assert_eq!(facts.len(), 3);

    let conformance = obligation(&changed, DEBIT_SERIALIZABLE);

    assert!(!conformance.conformant);
    assert_eq!(
        conformance.blockers,
        [Blocker {
            declaration: key(DEBIT),
            condition: ConditionKind::MissingFacts,
            facts: required(&changed, DEBIT, ConformanceClass::Attestable),
        }]
    );

    // Until re-attested.
    attest(
        &dir,
        &serializable,
        DEBIT,
        Attest::facts(required(&changed, DEBIT, ConformanceClass::Attestable))
            .reviewed_by(reviewer()),
    );

    let reattested = derive(&dir, &serializable);

    assert_eq!(kinds(&reattested, DEBIT), []);
    assert!(obligation(&reattested, DEBIT_SERIALIZABLE).conformant);
}

#[test]
fn the_summary_counts_declarations_and_names_each_blocker() {
    let dir = temp_dir("summary");
    wallet_code(&dir);

    let committed = wallet("read_committed");
    let before = derive(&dir, &committed);

    attest(
        &dir,
        &committed,
        DEBIT,
        Attest::facts(required(&before, DEBIT, ConformanceClass::Attestable))
            .reviewed_by(reviewer()),
    );

    attest(
        &dir,
        &committed,
        ACCOUNT,
        Attest::facts(Vec::new())
            .claims(vec![writers(&ACCOUNT_WRITERS)])
            .reviewed_by(reviewer()),
    );

    assert_eq!(
        derive(&dir, &committed).summary(),
        "declarations   10 bindable: 9 current, 1 unbound\n\
         obligations    1 proven: 1 conformant, 0 not conformant\n"
    );

    // Edit the debit's code.
    let path = dir.join("src/wallet.rs");
    let code = fs::read_to_string(&path).unwrap();
    fs::write(
        &path,
        code.replace("account.balance - amount", "account.balance - amount - 0"),
    )
    .unwrap();

    assert_eq!(
        derive(&dir, &wallet("serializable")).summary(),
        "declarations   10 bindable: 8 current, 1 unbound, 1 stale, 1 drifted, 1 missing_facts, \
         1 surplus\n\
         obligations    1 proven: 0 conformant, 1 not conformant\n\
         \x20 ! SerializableBy(input.debit.request.account_id) on tx.debit\n\
         \x20     missing_facts  transaction operation.debit/tx.debit\n\
         \x20                    isolation(tx.debit) = serializable\n"
    );
}

#[test]
fn the_state_is_a_function_of_its_inputs() {
    let dir = temp_dir("determinism");
    wallet_code(&dir);

    let committed = wallet("read_committed");

    let first = serde_json::to_string(&derive(&dir, &committed)).unwrap();
    let second = serde_json::to_string(&derive(&dir, &committed)).unwrap();

    assert_eq!(first, second);
}

// ---------------------------------------------------------------------
// §18.6 Completeness
// ---------------------------------------------------------------------

const ORDER: &str = "data_object data.checkout/object.order";
const CANCEL: &str = "transaction operation.cancel_order/tx.cancel_order";
const APPLY_SERIALIZABLE: &str =
    "SerializableBy(input.apply_payment.captured.order_id) on tx.apply_payment";

const ORDER_WRITERS: [&str; 1] = [r"(?i)\b(update|delete\s+from|insert\s+into)\s+orders\b"];

/// The checkout's code: the orders table, and the cancellation, a
/// declared writer of orders, inside its program.
fn checkout_code(dir: &Path) {
    put(
        dir,
        "migrations/0001_orders.sql",
        "-- conseqa:begin data_object data.checkout/object.order\n\
         CREATE TABLE orders (order_id uuid PRIMARY KEY, status text, version bigint);\n\
         -- conseqa:end data_object data.checkout/object.order\n",
    );

    put(
        dir,
        "src/cancel.rs",
        "// conseqa:begin program operation.cancel_order\n\
         pub fn cancel(db: &mut Db, order_id: Uuid) -> Result<()> {\n\
         \x20   // conseqa:begin transaction operation.cancel_order/tx.cancel_order\n\
         \x20   db.execute(\"UPDATE orders SET status = 'cancelled' WHERE order_id = $1\", order_id)?;\n\
         \x20   // conseqa:end transaction operation.cancel_order/tx.cancel_order\n\
         \x20   Ok(())\n\
         }\n\
         // conseqa:end program operation.cancel_order\n",
    );
}

fn order_claim(dir: &Path, workspace: &WorkspaceState, exceptions: Vec<Exception>) {
    let mut claim = writers(&ORDER_WRITERS);
    claim.exceptions = exceptions;

    attest(
        dir,
        workspace,
        ORDER,
        Attest::facts(Vec::new()).claims(vec![claim]),
    );
}

fn unheld(state: &State) -> Vec<Match> {
    let Condition::Incomplete { scopes } = condition(state, ORDER, ConditionKind::Incomplete)
    else {
        unreachable!()
    };

    assert_eq!(scopes.len(), 1);
    assert_eq!(scopes[0].scope, ClaimScope::Writers);

    match &scopes[0].failure {
        ScopeFailure::Unheld { claims } => {
            assert_eq!(claims.len(), 1);
            assert_eq!(claims[0].index, 0);
            assert_eq!(claims[0].more, 0);

            claims[0].matches.clone()
        }

        ScopeFailure::Absent => panic!("the claim is carried"),
    }
}

#[test]
fn a_writers_claim_is_rechecked_on_every_derivation() {
    let dir = temp_dir("no-deleters");
    let checkout = fixture("flash_checkout.yaml");

    checkout_code(&dir);

    // `no_deleters` is relied on, so the order's writers must be
    // claimed.
    let before = derive(&dir, &checkout);

    let scopes = &declaration(&before, ORDER).claims;
    assert_eq!(scopes.len(), 1);
    assert_eq!(scopes[0].scope, ClaimScope::Writers);
    assert!(
        displayed(&scopes[0].because).contains(&"no_deleters(object.order)".to_string()),
        "{:?}",
        displayed(&scopes[0].because)
    );

    // A match inside a declared writer's region is accepted.
    order_claim(&dir, &checkout, Vec::new());

    let claimed = derive(&dir, &checkout);
    assert!(!declaration(&claimed, ORDER).has(ConditionKind::Incomplete));

    // A new match outside every declared writer is not, and is listed.
    put(
        &dir,
        "scripts/purge.sql",
        "-- Nightly cleanup.\n\
         DELETE FROM orders\n\
         WHERE status = 'cancelled';\n",
    );

    let purged = derive(&dir, &checkout);

    assert_eq!(
        unheld(&purged),
        [Match {
            path: "scripts/purge.sql".to_string(),
            lines: [2, 2],
        }]
    );

    let blocker = obligation(&purged, APPLY_SERIALIZABLE)
        .blockers
        .iter()
        .find(|blocker| blocker.condition == ConditionKind::Incomplete)
        .expect("the incomplete claim blocks the proof resting on it");

    assert_eq!(blocker.declaration, key(ORDER));
    assert!(displayed(&blocker.facts).contains(&"no_deleters(object.order)".to_string()));

    // An exception restores it.
    order_claim(
        &dir,
        &checkout,
        vec![Exception {
            path: "scripts/purge.sql".to_string(),
            reason: "cancelled orders only, which no proof reads".to_string(),
        }],
    );

    let excepted = derive(&dir, &checkout);
    assert!(!declaration(&excepted, ORDER).has(ConditionKind::Incomplete));
    assert!(
        !obligation(&excepted, APPLY_SERIALIZABLE)
            .blockers
            .iter()
            .any(|blocker| blocker.condition == ConditionKind::Incomplete)
    );

    // A program is no writer: a match in its own lines lies outside the
    // transaction's region.
    let path = dir.join("src/cancel.rs");
    let code = fs::read_to_string(&path).unwrap();

    fs::write(
        &path,
        code.replace(
            "    Ok(())\n",
            "    db.execute(\"DELETE FROM orders WHERE order_id = $1\", order_id)?;\n    Ok(())\n",
        ),
    )
    .unwrap();

    assert_eq!(
        unheld(&derive(&dir, &checkout)),
        [Match {
            path: "src/cancel.rs".to_string(),
            lines: [6, 6],
        }]
    );

    // So does a match in a file with a scan error, whose regions are
    // void.
    fs::write(
        &path,
        code.replace("    Ok(())\n", "    // conseqa:begin\n    Ok(())\n"),
    )
    .unwrap();

    let broken = derive(&dir, &checkout);

    assert_eq!(broken.scan_errors.len(), 1);
    assert_eq!(
        unheld(&broken),
        [Match {
            path: "src/cancel.rs".to_string(),
            lines: [4, 4],
        }]
    );
}

#[test]
fn a_required_claim_that_is_absent_is_incomplete() {
    let dir = temp_dir("absent");
    let checkout = fixture("flash_checkout.yaml");

    checkout_code(&dir);

    // No record at all: nothing is claimed.
    let unrecorded = derive(&dir, &checkout);

    let Condition::Incomplete { scopes } = condition(&unrecorded, ORDER, ConditionKind::Incomplete)
    else {
        unreachable!()
    };

    assert_eq!(scopes[0].failure, ScopeFailure::Absent);
    assert_eq!(
        kinds(&unrecorded, ORDER),
        [ConditionKind::Unattested, ConditionKind::Incomplete]
    );

    // A record carrying no claim of the scope.
    attest(&dir, &checkout, ORDER, Attest::facts(Vec::new()));

    let recorded = derive(&dir, &checkout);

    let Condition::Incomplete { scopes } = condition(&recorded, ORDER, ConditionKind::Incomplete)
    else {
        unreachable!()
    };

    assert_eq!(scopes[0].failure, ScopeFailure::Absent);
}

#[test]
fn a_match_lies_in_a_region_only_strictly_between_its_markers() {
    let claim = CompletenessClaim {
        scope: ClaimScope::Writers,
        patterns: vec![
            r"(?i)delete\s+from\s+orders".to_string(),
            "(?m)^".to_string(),
            r"tx\.cancel_order".to_string(),
        ],
        exceptions: vec![Exception {
            path: "tools/backfill.rs".to_string(),
            reason: "one-off".to_string(),
        }],
    };

    let code = "// conseqa:begin transaction operation.cancel_order/tx.cancel_order\n\
                DELETE FROM\n\
                \x20 orders WHERE order_id = $1;\n\
                // conseqa:end transaction operation.cancel_order/tx.cancel_order\n";

    let mut matcher = Matcher::new([&claim]).unwrap();

    matcher.visit("src/cancel.rs", code);
    matcher.visit("tools/backfill.rs", "delete from orders;\n");
    matcher.visit("src/other.rs", "\n\nnothing here\n");

    let matches = matcher.into_matches().remove(0);

    // A match spans the lines it touches, and a marker line is text like
    // any other. The empty matches of `^` touch no text, and are passed
    // over.
    let at = |path: &str, lines: [usize; 2]| Match {
        path: path.to_string(),
        lines,
    };

    assert_eq!(
        matches,
        [
            at("src/cancel.rs", [1, 1]),
            at("src/cancel.rs", [2, 3]),
            at("src/cancel.rs", [4, 4]),
            at("tools/backfill.rs", [1, 1]),
        ]
    );

    let (regions, errors) = markers::scan_file("src/cancel.rs", code);
    assert_eq!(errors, []);

    // The statement inside the region is accepted, and so is the
    // excepted file. The markers' own lines are not inside their region.
    assert_eq!(
        completeness::outside(&claim, &matches, &regions.iter().collect::<Vec<_>>()),
        [at("src/cancel.rs", [1, 1]), at("src/cancel.rs", [4, 4])]
    );
}

#[test]
fn a_pattern_that_does_not_compile_is_refused_on_write_and_on_read() {
    let dir = temp_dir("pattern");
    let checkout = fixture("flash_checkout.yaml");

    checkout_code(&dir);

    let mut record = attest(&dir, &checkout, ORDER, Attest::facts(Vec::new()));
    record.completeness = vec![writers(&[r"(?i)\b(update|delete\s+from\s+orders"])];

    let records = Records::of_project(&dir);
    let digest = records.read(&key(ORDER)).unwrap().unwrap().digest;

    let error = records.write(&record, Some(digest)).unwrap_err();
    assert!(
        matches!(&error, ConformanceError::Refused { message, .. }
            if message == "completeness[0].patterns[0] is not a regular expression: unclosed group"),
        "{error}"
    );

    // A hand edit that breaks a pattern fails the read, naming it.
    let path = records.path(&key(ORDER));
    let text = fs::read_to_string(&path).unwrap();

    fs::write(
        &path,
        text.replace(
            "completeness: []",
            "completeness:\n- scope: writers\n  patterns:\n  - '(orders'",
        ),
    )
    .unwrap();

    let error = state::derive(&dir, &checkout).unwrap_err();

    assert!(
        matches!(&error, ConformanceError::Parse { message, .. }
            if message.contains("completeness[0].patterns[0] is not a regular expression")),
        "{error}"
    );
}

// ---------------------------------------------------------------------
// §10.1 Conditions
// ---------------------------------------------------------------------

const APPLY: &str = "transaction operation.apply_payment/tx.apply_payment";
const APPLY_IDEMPOTENCY: &str =
    "Idempotency(input.apply_payment.captured.event_id) on operation.apply_payment";

fn apply_code(dir: &Path) {
    put(
        dir,
        "src/apply.rs",
        "// conseqa:begin transaction operation.apply_payment/tx.apply_payment\n\
         pub fn apply(db: &mut Db, captured: &PaymentCaptured) -> Result<()> {\n\
         \x20   todo!()\n\
         }\n\
         // conseqa:end transaction operation.apply_payment/tx.apply_payment\n",
    );
}

#[test]
fn structural_facts_ask_their_bindable_anchors_for_the_structural_claim() {
    let dir = temp_dir("structural");
    let checkout = fixture("flash_checkout.yaml");

    apply_code(&dir);

    let before = derive(&dir, &checkout);

    // The payment's commit recovers the artifacts a retry needs, which
    // follows from the transaction's content: its structural claim must
    // provide it.
    let structural = displayed(&required(&before, APPLY, ConformanceClass::Structural));
    assert!(
        structural
            .iter()
            .any(|fact| fact.starts_with("artifact_route(tx.apply_payment")),
        "{structural:?}"
    );

    let Condition::MissingFacts {
        structural: missing,
        ..
    } = condition(&before, APPLY, ConditionKind::MissingFacts)
    else {
        unreachable!()
    };

    assert_eq!(displayed(missing), structural);

    // A key component's root is anchored to its requirement alone,
    // which no code implements: it asks nothing.
    assert!(
        before
            .declarations
            .iter()
            .flat_map(|state| &state.required)
            .all(|required| !required.fact.to_string().ends_with("by key_component"))
    );

    // Claimed, the structural facts hold.
    attest(
        &dir,
        &checkout,
        APPLY,
        Attest::facts(required(&before, APPLY, ConformanceClass::Attestable)).structural(),
    );

    let claimed = derive(&dir, &checkout);

    assert!(!declaration(&claimed, APPLY).has(ConditionKind::MissingFacts));
    assert!(
        !obligation(&claimed, APPLY_IDEMPOTENCY)
            .blockers
            .iter()
            .any(|blocker| blocker.declaration == key(APPLY))
    );

    // Checked against another residual, the claim says nothing of this
    // one.
    let records = Records::of_project(&dir);
    let file = records.read(&key(APPLY)).unwrap().unwrap();
    let mut record = record_of(&dir, APPLY);
    record.residual = json!({ "isolation": "serializable" });
    records.write(&record, Some(file.digest)).unwrap();

    let stale = derive(&dir, &checkout);

    let blocker = obligation(&stale, APPLY_IDEMPOTENCY)
        .blockers
        .iter()
        .find(|blocker| blocker.declaration == key(APPLY))
        .expect("the stale structural claim blocks the proof");

    assert_eq!(blocker.condition, ConditionKind::Stale);
    assert!(
        displayed(&blocker.facts)
            .iter()
            .all(|fact| fact.starts_with("artifact_route") || fact.starts_with("stable_root"))
    );
}

fn record_of(dir: &Path, declaration: &str) -> Record {
    match Records::of_project(dir)
        .read(&key(declaration))
        .unwrap()
        .unwrap()
        .content
    {
        conseqa::conformance::record::RecordContent::Read(record) => *record,
        other => panic!("{declaration}'s record was set aside: {other:?}"),
    }
}

#[test]
fn a_declaration_owes_its_facts_with_no_record_whatever_the_policy_binds() {
    let dir = temp_dir("owed");
    let state = derive(&dir, &fixture("flash_checkout.yaml"));

    // A transition implementing a message identity owes it, though the
    // policy requires no transition bound.
    let transition = state
        .declarations
        .iter()
        .find(|state| {
            state.declaration.key().kind() == "transition"
                && state
                    .required
                    .iter()
                    .any(|required| kind_of(&required.fact) == "message_identity")
        })
        .expect("a checkout transition publishes a schema whose identity is relied on");

    assert_eq!(
        transition.declaration.to_string(),
        "transition machine.order_lifecycle/transition.order.mark_paid"
    );

    assert_eq!(
        transition
            .conditions
            .iter()
            .map(Condition::kind)
            .collect::<Vec<_>>(),
        [ConditionKind::MissingFacts]
    );

    // A transaction, which the policy binds, is unbound and owes its
    // facts too.
    let transaction = state
        .declarations
        .iter()
        .find(|state| {
            state.declaration.key().kind() == "transaction"
                && state
                    .required
                    .iter()
                    .any(|required| required.class == ConformanceClass::Attestable)
        })
        .unwrap();

    assert_eq!(
        transaction
            .conditions
            .iter()
            .map(Condition::kind)
            .collect::<Vec<_>>(),
        [ConditionKind::Unbound, ConditionKind::MissingFacts]
    );
}

#[test]
fn a_record_of_another_version_leaves_its_declaration_unattested() {
    let dir = temp_dir("set-aside");
    let checkout = fixture("flash_checkout.yaml");

    apply_code(&dir);
    attest(&dir, &checkout, APPLY, Attest::facts(Vec::new()));

    let path = Records::of_project(&dir).path(&key(APPLY));
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, text.replace("dsl: 6", "dsl: 5")).unwrap();

    let state = derive(&dir, &checkout);

    let Condition::Unattested { set_aside } = condition(&state, APPLY, ConditionKind::Unattested)
    else {
        unreachable!()
    };

    let set_aside = set_aside.as_ref().expect("the record was set aside");

    assert_eq!(
        set_aside.record,
        ".conseqa/conformance/transaction/operation.apply_payment/tx.apply_payment.yaml"
    );
    assert!(set_aside.reason.contains("dsl 5"), "{}", set_aside.reason);
    assert!(set_aside.reason.contains("dsl 6"), "{}", set_aside.reason);

    // Read as nothing: every required fact is missing, and nothing is
    // stale, drifted, or surplus.
    assert_eq!(
        kinds(&state, APPLY),
        [ConditionKind::Unattested, ConditionKind::MissingFacts]
    );
}

#[test]
fn drift_counts_region_digests_as_a_multiset() {
    let dir = temp_dir("drift");
    let checkout = fixture("flash_checkout.yaml");

    apply_code(&dir);
    attest(&dir, &checkout, APPLY, Attest::facts(Vec::new()));

    assert!(!declaration(&derive(&dir, &checkout), APPLY).has(ConditionKind::Drifted));

    // Moved to another file unchanged: the same code, so no drift.
    let code = fs::read_to_string(dir.join("src/apply.rs")).unwrap();
    fs::remove_file(dir.join("src/apply.rs")).unwrap();
    put(&dir, "src/payments/apply.rs", format!("// moved\n{code}"));

    assert!(!declaration(&derive(&dir, &checkout), APPLY).has(ConditionKind::Drifted));

    // A second region of the same code: the multiset grew.
    put(&dir, "src/payments/again.rs", &code);

    let grown = derive(&dir, &checkout);

    let Condition::Drifted { added, removed } = condition(&grown, APPLY, ConditionKind::Drifted)
    else {
        unreachable!()
    };

    assert_eq!(added.len(), 1);
    assert_eq!(removed, &[]);

    // An edited region: the recorded one is gone, the edited one new.
    fs::remove_file(dir.join("src/payments/again.rs")).unwrap();
    put(
        &dir,
        "src/payments/apply.rs",
        format!("// moved\n{}", code.replace("todo!()", "unimplemented!()")),
    );

    let edited = derive(&dir, &checkout);

    let Condition::Drifted { added, removed } = condition(&edited, APPLY, ConditionKind::Drifted)
    else {
        unreachable!()
    };

    assert_eq!(added[0].path, "src/payments/apply.rs");
    assert_eq!(added[0].lines, [2, 6]);
    assert_eq!(removed[0].path, "src/apply.rs");
}

#[test]
fn keys_naming_no_declaration_are_orphaned() {
    let dir = temp_dir("orphaned");
    let checkout = fixture("flash_checkout.yaml");

    put(
        &dir,
        "src/legacy.rs",
        "// conseqa:begin transaction operation.refund/tx.refund\n\
         fn refund() {}\n\
         // conseqa:end transaction operation.refund/tx.refund\n",
    );

    // A record about a declaration the spec no longer has.
    let mut record = Record::new(
        key("effect_site operation.refund/effect.notify"),
        json!({}),
        Attestation {
            by: attester(),
            commit: None,
        },
    );
    record.structural = true;
    write(&dir, &record);

    let state = derive(&dir, &checkout);

    assert_eq!(state.scan_errors, []);
    assert_eq!(
        state
            .orphaned
            .iter()
            .map(|orphan| (
                orphan.key.to_string(),
                orphan.regions.len(),
                orphan.record.clone()
            ))
            .collect::<Vec<_>>(),
        [
            (
                "transaction operation.refund/tx.refund".to_string(),
                1,
                None
            ),
            (
                "effect_site operation.refund/effect.notify".to_string(),
                0,
                Some(
                    ".conseqa/conformance/effect_site/operation.refund/effect.notify.yaml"
                        .to_string()
                )
            ),
        ]
    );

    assert_eq!(state.counts()[&ConditionKind::Orphaned], 2);
    assert!(
        state
            .summary()
            .contains("orphaned       2 keys: transaction operation.refund/tx.refund, ")
    );
}

#[test]
fn only_another_party_at_the_current_digests_confirms_a_review() {
    let dir = temp_dir("reviews");
    wallet_code(&dir);

    let committed = wallet("read_committed");
    let facts = required(
        &derive(&dir, &committed),
        DEBIT,
        ConformanceClass::Attestable,
    );

    attest(
        &dir,
        &committed,
        ACCOUNT,
        Attest::facts(Vec::new())
            .claims(vec![writers(&ACCOUNT_WRITERS)])
            .reviewed_by(reviewer()),
    );

    let unreviewed = |state: &State| -> Vec<ReviewTarget> {
        match declaration(state, DEBIT)
            .conditions
            .iter()
            .find(|condition| condition.kind() == ConditionKind::Unreviewed)
        {
            Some(Condition::Unreviewed { items }) => items.clone(),
            _ => Vec::new(),
        }
    };

    let every_fact = vec![
        ReviewTarget::Fact(0),
        ReviewTarget::Fact(1),
        ReviewTarget::Fact(2),
    ];

    // The attester reviewing itself confirms nothing, and neither does a
    // checker, which covers no proof fact.
    attest(
        &dir,
        &committed,
        DEBIT,
        Attest::facts(facts.clone())
            .reviewed_by(attester())
            .reviewed_by(party(
                PartyKind::Checker,
                "postgres.database/conditional-update@1",
            )),
    );

    assert_eq!(unreviewed(&derive(&dir, &committed)), every_fact);

    // Another party's confirmation does.
    let mut record = attest(
        &dir,
        &committed,
        DEBIT,
        Attest::facts(facts.clone()).reviewed_by(reviewer()),
    );

    assert_eq!(unreviewed(&derive(&dir, &committed)), []);

    // An item's verdict overrides the verdict on the whole record.
    record.reviews[0].items = vec![ReviewItem {
        target: ReviewTarget::Fact(1),
        verdict: Verdict::Disputed,
        note: Some("the version is never compared".to_string()),
    }];
    write(&dir, &record);

    let disputed = derive(&dir, &committed);

    assert_eq!(unreviewed(&disputed), [ReviewTarget::Fact(1)]);

    let Condition::Disputed { reviews } = condition(&disputed, DEBIT, ConditionKind::Disputed)
    else {
        unreachable!()
    };

    assert_eq!(reviews.len(), 1);
    assert_eq!(reviews[0].by, reviewer());
    assert!(!reviews[0].record);
    assert_eq!(reviews[0].items, [ReviewTarget::Fact(1)]);

    // A dispute does not make the obligation unconformant: disputes are
    // gated apart.
    assert!(obligation(&disputed, DEBIT_SERIALIZABLE).conformant);

    // Once the code changes, the review saw other code: it neither
    // confirms nor disputes.
    let path = dir.join("src/wallet.rs");
    let code = fs::read_to_string(&path).unwrap();
    fs::write(&path, code.replace("SELECT *", "SELECT balance, version")).unwrap();

    let edited = derive(&dir, &committed);

    assert_eq!(unreviewed(&edited), every_fact);
    assert!(!declaration(&edited, DEBIT).has(ConditionKind::Disputed));

    // And the obligation's guarded mutation is attested of other code.
    let conformance = obligation(&edited, DEBIT_SERIALIZABLE);

    assert!(!conformance.conformant);
    assert_eq!(conformance.blockers[0].condition, ConditionKind::Drifted);
}

#[test]
fn an_obligation_relying_on_an_unconformant_one_names_it() {
    let dir = temp_dir("relies-on");
    let capture = fixture("payment_capture.yaml");
    let state = derive(&dir, &capture);

    let publish = obligation(
        &state,
        "Idempotency(input.publish_payment_event.outbox.event_id) on operation.publish_payment_event",
    );

    assert!(!publish.conformant);
    assert_eq!(
        publish
            .through
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        [
            "Idempotency(input.post_ledger_entry.captured.event_id) on operation.post_ledger_entry",
            "Idempotency(input.send_receipt.captured.event_id) on operation.send_receipt",
        ]
    );
}

#[test]
fn a_spec_that_does_not_verify_cannot_be_derived() {
    let dir = temp_dir("unverifiable");
    let mut checkout = fixture("flash_checkout.yaml");

    checkout
        .operations
        .get_mut(&Id("operation.cancel_order".to_string()))
        .unwrap()
        .program = None;

    let error = state::derive(&dir, &checkout).unwrap_err();

    assert!(
        matches!(&error, ConformanceError::Unverifiable { .. }),
        "{error}"
    );
    assert!(
        error
            .to_string()
            .starts_with("the spec cannot be verified: ")
    );
}

#[test]
fn a_project_derives_from_its_store() {
    let dir = temp_dir("store");
    let store = ProjectStore::create(&dir, &fixture("flash_checkout.yaml")).unwrap();
    drop(store);

    checkout_code(&dir);

    let workspace = load_source(&ProjectDirectory::of_project(&dir))
        .unwrap()
        .unwrap();

    let state = derive(&dir, &workspace);

    // The spec's own files under `.conseqa/` are never scanned.
    assert_eq!(state.scan_errors, []);
    assert_eq!(declaration(&state, CANCEL).regions.len(), 1);
    assert!(
        declaration(&state, CANCEL).regions[0]
            .path
            .starts_with("src/")
    );
}
