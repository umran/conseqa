# Conseqa System One Orchestration Revision

**Target baseline:** `236ffcc0c535ffef727b19bbd4cbf278bac822fa`  
**DSL contract:** unchanged (`DSL_VERSION = 4`)  
**Report format:** unchanged (`FORMAT = 7`)  
**Persistence format:** unchanged (`FORMAT = 6`)  
**Amends:** `CONSEQA_AGENT_CONFLUENCE_HARNESS_IMPLEMENTATION_SPEC.md` §40, §44, §50, §55, §74.1, §74.2, §87, §88

## 1. Purpose

This revision does two things.

First, it brings the confluence MCP surface, its embedded documentation, and the harness specification into agreement with DSL v4: transaction-level serializability and ordering, and an L1 that describes placement, transport and capacity and proves no transaction property.

Second, it moves authoring decisions out of opaque LLM agent sessions and into code, wherever the decision space is closed. Code owns the control flow. A *System One* model answers narrow closed-set questions. A generative model supplies open-set content and handles what the first two decline. The analyzer remains the only authority on correctness.

The revision introduces:

- a self-sufficient JSON authoring contract in `dsl_reference`;
- a strict `requirement_report` family filter;
- a corrected conflict footprint for transaction-family requirement proposals;
- per-task run telemetry and a decision log;
- a `Decider` primitive interface with hosted, local and replay backends, and a shadow mode that answers every question with two backends;
- an in-process System One executor at the existing `AgentBackend` seam, with abstention to the agent backend;
- a non-committing candidate evaluation entry point on the engine;
- System One builders for requirement discovery, requirement repair, runtime topology and, later, operation synthesis;
- advisory MCP tools that return analyzer-verified candidate patches.

The revision removes:

- guidance promising a `runtime` remedy no prover produces;
- every remaining statement that L1 discharges a serializability or ordering requirement;
- harness specification §74.1 and §74.2 as written.

The governing principle becomes:

> Code owns control flow. A System One model chooses among options code has enumerated. A generative model writes what cannot be enumerated. The analyzer alone decides whether a specification is correct.

---

# 2. Why the harness cannot simply use a faster model

## 2.1 Current execution model

Every harness task is one external agent session: `claude -p` or `codex exec`, launched by an `AgentBackend` (`src/harness/backend.rs`). The session owns its own agentic loop. It reads `dsl_reference`, reads symbols, queries the graph, reasons, and calls `submit_patch`, typically over dozens of turns.

The harness contains no tool-call loop. It observes tool *names* only (`AgentEvent::ToolCall { name }`), never arguments, and it reads the architectural outcome from the engine, never from the agent.

Consequently there is no decision point inside the harness into which a faster model could be substituted. Every authoring decision is taken inside a session the harness cannot see into.

## 2.2 What a System One model is

A System One model evaluates one `state` against a map of typed questions and returns typed answers with probabilities:

```text
Choice   pick one option from a supplied set    -> choice, probabilities, confidence
Score    place the state on an ordered rubric   -> score, probabilities, confidence
Noul     is this statement true                 -> probability of yes
```

All questions of a request are evaluated independently and in parallel against a single ingestion of the state. A request answers in roughly 0.1–0.5 seconds almost regardless of question count.

It is not an agent. It does not choose its own next action, and it cannot generate: a value outside a supplied option set has no question that could produce it.

## 2.3 Consequence

The speed available is not the speed of a faster session. It is the speed of having no session: a task executed by code, which asks a System One model only the questions code cannot answer, and which falls back to an agent session when it cannot proceed.

Conseqa is unusually suited to this:

- most patch arguments are references into the symbol graph, which code can enumerate as an option set;
- the vocabulary of steps, isolation levels, lock modes, cursor rules, delivery semantics and requirement families is closed;
- an unproven obligation already names its gaps (`LockCoverageMissing`, `VersionValidationMissing`, `IsolationUnspecified`, ...), so "how is this repaired" is a finite menu of program transformations;
- the analyzer is a sound, deterministic verifier, so a wrong preference costs a suboptimal but proven specification, never an unsound one.

---

# Part I — Contract refresh

# 3. `requirement_report` family filter

## 3.1 Current

`RequirementReportParams.family` is documented as:

```text
serialization, ordering, idempotency, result_replay, recoverability
```

`property_matches` (`src/confluence/engine.rs`) accepts:

```text
transaction_serializability, transaction_ordering, idempotency, result_replay, recoverability
```

Passing the documented `serialization` or `ordering` matches no obligation. The tool returns an empty obligation list with no error. An agent concludes that nothing is open.

## 3.2 Revised

The documented values are exactly the accepted values.

An unrecognized family is an error, never an empty result:

```json
{ "error": "unknown_requirement_family",
  "family": "serialization",
  "accepted": ["transaction_serializability", "transaction_ordering",
               "idempotency", "result_replay", "recoverability"] }
```

A custom property name remains accepted when the model declares it.

There is no alias for the v3 names.

---

# 4. Conflict footprint of transaction-family proposals

## 4.1 Current

DSL v4 moved serializability and ordering requirements onto `Transaction.requirements`, inside the operation program.

`adopt_requirement` (`src/confluence/commit.rs`) therefore writes the program:

```rust
draft.program
    .and_then(|program| program.transaction_mut(transaction))
    ...
    .requirements.serializability.push(requirement)
```

`Mutation::write_target` (`src/confluence/patch.rs`) still reports only:

```rust
Self::ProposeRequirements { operation, .. } => SymbolKey::OperationRequirements(operation)
```

So for a transaction-family proposal:

- write-write conflict detection (commit protocol step 7, over `patch.write_targets()`) never sees `OperationProgram(operation)`;
- the scheduler footprint records only `OperationRequirements(operation)`, and may place the proposal in one wave with a task that replaces the same program.

The program fingerprint does change, so dependents are invalidated after the fact. The defect is masked only because requirement discovery never runs concurrently with program synthesis. This revision removes that guarantee by making tasks cheap and their phases eager.

## 4.2 Revised

Authorization and conflict footprint are separated.

**Authorization** is unchanged. `ProposeRequirements` is authorized by an `OperationRequirements(operation)` grant, whatever the family. A requirement-discovery task gains no authority to replace a program.

**Conflict footprint** is the set of symbols whose fingerprint the mutation may change:

```rust
ProposeRequirements, any proposal of a transaction family
    => { OperationRequirements(operation), OperationProgram(operation) }

ProposeRequirements, otherwise
    => { OperationRequirements(operation) }
```

`SpecPatch::write_targets` and the scheduler footprint both use the conflict footprint.

`ReplaceOperationRequirements` is audited under the same rule.

## 4.3 Non-implication

This does not make requirement discovery a program writer in the scope model. It records that adopting a transaction requirement changes the program's fingerprint, which is what conflict detection is about.

## 4.4 Obligation reads

A second conflict in the same phase runs the other way: not a write that is missed, but reads that are too wide.

A prompt obligation's fingerprint covers its `status`, and adopting an `ExplicitPrompt` proposal rewrites the obligation it names to `mapped`. Requirement discovery fans out one task per operation, and nothing offers a task the obligations aimed at *its* operation: the context bundle carries none, so a worker lists every obligation and reads each. The first task to map its obligation therefore invalidates every peer that had read it — a cancelled and restarted session each, in the agent-only baseline as much as under this revision.

`SearchSpec` gains `targets: Option<Id>`: prompt obligations targeting one operation, and no other kind of symbol. The observation is, as for every search, the fingerprint of the returned set. So a task that searches with `targets` and reads what is returned is

- not invalidated when a peer maps an obligation aimed elsewhere — it observed neither that obligation nor a set containing it;
- invalidated when an obligation is aimed at, or away from, its operation — a phantom — or when one of its own is rewritten.

`search_symbols` publishes the parameter, the requirement-discovery prompt directs a session to it, and the discovery builder (§17) uses it. An absent `targets` serializes as before, so no recorded observation changes.

---

# 5. The `runtime` remedy

## 5.1 Current

`RemedyLayer` has two values, `Application` and `Runtime`. Exactly two provers set a remedy — transaction serializability and transaction ordering — and both return `Application` for every unproven verdict. Idempotency, result-replay and recoverability obligations carry no remedy.

`INTERACTIVE_INSTRUCTIONS` nevertheless instructs agents to route on an `application` versus `runtime` remedy.

## 5.2 Revised

The variant is retained. Report format 7 is unchanged, and no emitted report ever carried the value.

The guidance is corrected to state the actual contract:

- an unproven transaction serializability or ordering obligation always carries `remedy: application`;
- no obligation currently carries `remedy: runtime`;
- no L1 edit can discharge a transaction obligation.

Whether a replay-family obligation whose only missing premise is a delivery fact should produce `Runtime` is recorded as an open item (§35). Until a prover produces it, no agent is told to expect it.

---

# 6. `dsl_reference` is a complete JSON authoring contract

## 6.1 Current

`DSL_REFERENCE` is the only JSON-shaped contract a worker receives. `submit_patch` accepts JSON. The fallback, `dsl_guide`, serves `CONSEQA_DSL_SEMANTICS.md`, whose examples are YAML.

The reference omits:

- the object selector — `<object selector>` appears as an undefined placeholder;
- `SelectorPredicate` (`all`, `eq`, `and`) and `SelectorValue`;
- `FieldSelection` (`all`, `only`);
- the `read`, `write`, `insert` and `delete` step shapes;
- the `TransactionIsolation` values as an enumeration;
- the rule that a `validate_version` selector pins every identity field (`VersionValidationWithoutIdentifiedInstance`).

A worker that authors a version guard from the reference alone writes a selector the gate rejects, with no forewarning.

## 6.2 Revised

The reference is sufficient, alone, to author every `Mutation` kind, every `OperationStep` kind and every `TransactionStep` kind in JSON.

It states every validation rule whose violation is a first-submission rejection rather than a design choice. At minimum:

- `validate_version` names an observed version and selects one identified instance;
- every `write` or `transition` of a versioned instance is matched by exactly one `bump_version`;
- a transaction body containing a rejecting step declares a `rejected` arm, and no other does;
- a `write` never names a version or managed field.

## 6.3 Examples are data

Every JSON example in the reference is a typed value held in code and rendered into the text. Each is parsed into its Rust type and, where it is a program or a transaction, passed through `program_local_diagnostics` by a test, as `PROGRAM_EXAMPLE_JSON` is today.

An example that does not parse or validate fails the build. The reference cannot drift from the types.

## 6.4 A schema facility, not a wider handshake

The `patch` parameter of `submit_patch` remains published as an opaque JSON value. Publishing the full `SpecPatch` schema in the tool's input schema would charge every MCP client the entire DSL vocabulary at handshake.

Instead the typed schema is available on demand:

```text
dsl_schema { kind?: <mutation kind> | <step kind> }
    -> the JSON Schema of that value, derived from the Rust type
```

Spec types derive `schemars::JsonSchema` under the `confluence` feature only. The lean checker gains no dependency.

`coerce_stringified_json` is retained as tolerance.

---

# 7. Stale v3 rationale

The following are rewritten. None changes behaviour.

| Location | Stale claim |
| --- | --- |
| `src/confluence/task.rs`, `shared_skeleton` doc | L1 exists to discharge serialization and ordering requirements |
| `src/confluence/commit.rs`, skeleton diagnostics doc | L1 is authored once verification has said what it must discharge |
| `tests/harness_workflow.rs`, topology test doc | the runtime topology is authored to discharge unproven obligations |
| harness spec §40 | `OperationSummary` carries `serialization` and `ordering` |
| harness spec §74.1 | repair is partitioned by remedy layer, with a runtime partition |
| harness spec §74.2 | the topology phase is entered from the unproven set and re-enters |

The correct statements, which the code already implements:

- L1 is authored exactly once per run, after L0 converges, because a complete specification needs a runtime realization — not because any obligation needs one;
- repair is partitioned per operation, and every transaction obligation's remedy is an application edit;
- L1 *structural* diagnostics are routed to the topology author. That is validation, not proof, and is retained.

---

# 8. Budgets are enforced uniformly

## 8.1 Current

The daemon's `request_design` caps a session at 600 seconds and 60 turns. `conseqa-harness design` applies no cap. `TaskBudget.max_tokens` and `TaskBudget.max_usd_cents` are carried into `task_context` and enforced nowhere.

## 8.2 Revised

Both entry points apply the same default invocation budget, overridable by flag.

The supervisor enforces `max_tokens` and `max_usd_cents` from `AgentEvent::Usage`: a session that exceeds either is cancelled and recorded as `TimedOut` with the budget named. A budget that is declared is a budget that is enforced.

---

# Part II — The System One layer

# 9. Three lanes

Every authoring decision belongs to exactly one lane.

| Lane | Owns | Never |
| --- | --- | --- |
| **Code** | control flow; enumeration of candidates from the symbol graph; identifier derivation; remedy synthesis; validation, verification, OCC | guesses intent |
| **System One** | preference among enumerated candidates; whether the prompt states a fact; fidelity of generated content to the prompt; triage | generates a value; decides correctness; widens a scope |
| **Generative** | names, schemas, fields and prose that cannot be enumerated; any task System One declines | bypasses the gate |

The lanes are ordered by cost. A decision is taken in the cheapest lane able to take it soundly.

---

# 10. Primitive interface

The types are the wire format, so a question serializes as it is sent and an answer deserializes as it arrives.

```rust
#[serde(tag = "type")]
pub enum Question {
    Choice { instructions: Json, criteria: BTreeMap<String, Json> },   // option -> description, or null
    Score  { instructions: Json, criteria: Vec<Json> },                // levels, lowest first
    Noul   { instructions: Json, criteria: Option<NoulCriteria> },     // what true and false mean
}

#[serde(tag = "type")]
pub enum Answer {
    Choice { choice: String, probabilities: BTreeMap<String, f64>, confidence: f64 },
    Score  { score: f64, legend: BTreeMap<String, Json>,
             probabilities: BTreeMap<String, f64>, confidence: f64 },  // keyed by level index
    Noul   { noul: f64 },
}

pub struct DecisionRequest {
    pub state: Json,
    pub questions: BTreeMap<QuestionId, Question>,
    pub tags: BTreeMap<String, String>,     // logged; never sent; no part of a replay key
}

pub struct Decision {
    pub identity: DeciderIdentity,          // backend, pinned model id, endpoint, calibrated?
    pub answered_by: String,                // the model id the backend says answered
    pub answers: BTreeMap<QuestionId, Answer>,
    pub usage: Option<Usage>,
    pub latency: Duration,                  // end to end, retries included
    pub shadow: Option<ShadowOutcome>,
}

#[async_trait]
pub trait Decider: Send + Sync {
    fn identity(&self) -> DeciderIdentity;
    async fn decide(&self, request: &DecisionRequest) -> Result<Decision, DeciderError>;
}
```

Every implementation returns only answers that *conform* to their questions: the same type, every option or level reported and none invented, a distribution that sums to one, a choice that is the most probable option, and every number a probability to within rounding. A backend that answers a different question from the one asked is refused, however healthy its transport, because a backend may be swapped for one that reproduces the shape and invents the numbers (§11.2).

`confidence` is the backend's own statistic and is not comparable across backends. `Answer::concentration` — one minus the normalized Shannon entropy of the distribution — is computed identically for every backend, and is what two backends are compared on.

Rules:

- a `Choice` has between 2 and 255 options, the limit of the wire format; a wider enumeration is ranked in chunks and re-ranked (§20.3);
- a `Score` has between 2 and 10 levels;
- a `Noul` carries no confidence;
- a threshold tuned for one question type is never applied to another, and a threshold tuned for one backend is never applied to another;
- the confidence of a composite decision is the minimum over the judgments it consumed, never their product; an unused speculative answer contributes nothing;
- confidence summarizes how concentrated a distribution is. It is neither correctness nor permission to act: several acceptable options spread probability as readily as ignorance does;
- a `Noul` near one half means yes and no are similarly probable, not that the condition holds to a medium degree;
- the two probabilities of a question and its negation are not assumed to sum to one.

`DeciderError` is never silently absorbed. A builder that cannot obtain an answer abstains (§16).

---

# 11. Backends

## 11.1 `SystemOneHttpDecider`

Speaks the System One wire format:

```text
POST {base_url}/v1/systemone
{ "state": ..., "model": ..., "questions": { <id>: <question> } }
    -> { "model": ..., "answers": { <id>: <answer> } }
```

The wire format is the contract, not the vendor. The same client addresses the hosted TypeSafe service and any local server that implements the format. Base URL, model id and credential are configuration, so the hosted and the local backend differ in nothing but their settings.

- The model id is pinned to a versioned id, never an alias. Thresholds are tuned per id.
- Retries mirror the vendor's own client: `408`, `429` and every `5xx`, connection failures and timeouts, twice, backing off from half a second to five with a quarter jittered, each attempt timing out at thirty seconds. `retry-after` is honoured up to a cap, so a server cannot stall a run. A `4xx` the backend will never accept is not retried.
- Request size is estimated from bytes, conservatively, because the backend's tokenizer is not available to the client. A loopback server is given no client-side budget; its limits are its own to report.
- Only the backend's failures open the circuit. A rejected request or a refused credential would fail identically against a healthy backend.
- A request is refused client-side when `state` plus all questions exceeds the configured request budget, or `state` plus the longest question exceeds the configured state budget (64k and 32k tokens for the hosted service). The builder must slice; the backend never truncates.
- A circuit breaker opens after consecutive failures. While open, builders abstain.
- The credential is read from the environment. It is never written to a prompt, the decision log, the manifest or the replay store.
- The vendor's live API reference is the source of truth for the wire format. It is read before the client is written, and again before a pinned model id is moved.

## 11.2 Admissible local servers

Reading option probabilities from a stock model's logits reproduces the *shape* and the *speed* of a System One model. It does not reproduce its training or its calibration, and an implementation can reproduce the shape while inventing the numbers.

A local server is admissible as a decider only when:

- every option, with its criteria, is presented to the model. An option the model was not shown is not an option;
- each answer's probabilities are a normalized readout over the full option set. No floor, no cap, and no mass assigned by rule rather than by the model;
- a question the server cannot resolve is an error, never a default option;
- questions are isolated: the text of one question is not context for another;
- `Choice`, `Score` and `Noul` are all supported;
- it reports the model and revision it serves, and whether its probabilities were calibrated.

Admissibility is established by a conformance suite run against the server (§32), never taken from its documentation. Conseqa specifies the suite; whether it also ships a reference server is an open item (§35).

Weights come from an identifiable publisher, in a non-executable format, at a pinned revision. An unreviewed server is not run.

## 11.3 `LogprobDecider`

Optional, and outside the minimum implementation sequence. For a local endpoint that speaks only an OpenAI-compatible completion API exposing token log-probabilities (`vLLM`, `llama.cpp` server, `Ollama`).

- Each question is one completion of one token over single-token option labels, at temperature zero, with `logprobs` requested. Probabilities are the softmax of the option labels' log-probabilities, renormalized over the option set.
- A `Noul` is a two-option choice; its answer is the probability of `yes`.
- A `Score` is a choice over level indices; its score is the expectation.
- Requests share the state as a common prefix, ordered so the server's prefix cache is reused.
- A `Choice` wider than the endpoint's top-logprob limit is evaluated as a chunked tournament, and its answers are marked `degraded` in the decision log.
- Confidence is `1 - H(p) / ln(n)`. It is uncalibrated until a temperature is fitted from the decision log (§13.3); until then the backend's identity reports `calibrated: false`.

## 11.4 `ReplayDecider`

Deterministic. A content-addressed store. Each answer is keyed by `blake3(model id, state, question)` — the question's own text, so a reworded question misses and a stale recording can never answer it. The question spec's version (§12) is what makes a rewording visible in the log; the text is what makes it safe.

Questions are keyed one by one, not request by request. That is sound because a System One backend answers each question in isolation (§11.2), and it keeps a recording useful when a builder later asks one more question of the same state.

- *Record* mode wraps another backend, asks it only for what the store lacks, and persists the answers atomically.
- The store retains each state and question as asked (§13.2), is validated when loaded — a recording that no longer answers its own question is refused — and never holds a credential.
- *Replay* mode answers from the store only. A miss is an error, never a live call.

Tests and CI use replay mode exclusively.

## 11.5 `ShadowDecider`

Two backends, one *primary* and one *shadow*. Every request is sent to both. Only the primary's answers drive behaviour. Both answers are written to the decision log with their agreement.

Shadow mode is how the hosted and local backends are compared on identical questions from the first day, without either being trusted on the other's behalf.

## 11.6 `PreviewDecider`

A hosted decider is data egress, and a run sends it the prompt. Before a run is allowed to, it can be run once with the preview decider in the wire client's place.

Every request a builder would have made is appended to a file as the body the wire client posts — the same serialized type, so what the file shows is what would have been sent — beside the tags that say who was asking. No request is made. Every ask fails, every builder abstains, and the run proceeds on its agent backend exactly as it would with the layer off.

It needs no URL and no credential, and it reports no egress. A request the wire client would have refused is not previewed as though it would have been sent.

The file is the whole answer to "what leaves this machine, and what is asked of it", readable before a credential is ever configured. It is also the material a question's wording is reviewed against: real state, from a real run, with no model in the loop.

## 11.7 Non-implications

- No backend is a default. With no decider configured, the System One layer is disabled and the harness behaves exactly as at baseline.
- A base URL that is not loopback is data egress: state slices — prompt excerpts, symbol ids, program fragments — leave the machine. It is enabled only by explicit configuration (§26), and there is no default URL.
- No backend is fine-tuned. Domain knowledge enters through the state and the question text.

---

# 12. Questions are reviewed data

Every question is a `QuestionSpec`: an id, a version, instructions, criteria, and the thresholds that consume its answer. All specs live in one module, `src/system_one/questions/`, so that the entire decision surface can be read in one place.

Authoring rules, enforced in review:

- one question asks one narrow, coherent judgment. Independently useful dimensions are split and combined in code, without splitting apart the relationship being judged: a bounded selection among candidates is one judgment, and narrow does not mean literal fact extraction;
- a question id is for code and is never seen by the model. The instructions and criteria carry the complete meaning;
- the question states its exact condition and its boundary cases in the criteria; it is answered as written, not as meant;
- nothing code can compute — a count, an ordering, a comparison of versions, positions or dates — is asked of the model;
- a question that reaches through more than one level of indirection is decomposed or not asked;
- every optional decision is preceded by a `stated` Noul — *does the prompt say anything about this at all* — and when the answer is no, the default stands;
- a `Choice` whose right answer may be missing from the enumeration carries an explicit no-match option, and code checks candidate coverage: the model cannot choose a value it was not offered;
- follow-up questions whose relevance depends on another answer are asked in the same request, speculatively, each stating its premise explicitly, and are ignored when irrelevant. A second request is made only when an earlier answer is needed to build new state or new options;
- the state contains only what the questions need.

Changing a spec's text bumps its version. The version is part of the replay key, so stale recordings cannot answer a reworded question.

---

# 13. Telemetry and the decision log

## 13.1 Run manifest

`confluence-manifest.json` today records a task *count*. It gains `manifest_format: 2` and one record per logical task:

```text
kind, operation?, executor (agent | system_one), backend,
attempts, final_state, exhausted,
wall_ms, turns?, tokens?, usd_cents?,
tool_calls: { <tool name>: count },
abstained?: <reason>
```

and per run: wall time to terminal status, sessions spawned, rejected submissions, obligations proven over total, prompt obligations mapped over total.

`executor` and `backend` describe whoever settled the final attempt. A task a builder abstained from is an `agent` task whose record carries `abstained`, so an escalation rate can be read off any run without the decision log. `wall_ms` runs from the creation of the first attempt to the final outcome, restarts included.

As built, the first increment: the scheduler keeps the ledger (`Scheduler::ledger`), one `LogicalTaskRecord` per logical task; `AgentExit` gains `escalation`, which the executor sets on the session it hands a task to; the manifest adds `wall_ms`, `task_records`, and `executors` — per executor, tasks, sessions, summed task wall time, and abstentions received. Task wall times overlap under concurrency, so the per-executor sum is work, not elapsed time. `tool_calls`, rejected submissions and the proven-over-total counters are not yet recorded.

## 13.2 Decision log

`decisions.jsonl`, one line per decision:

```text
decision id, task, builder, question spec id and version,
backend identity, state hash, full probabilities, confidence,
threshold applied, action taken,
shadow: { backend identity, probabilities, confidence, agrees }?
```

The state hash and the question version address the exact request in the replay store, which retains the canonical state and question text. Any decision can be reproduced and inspected as it was asked.

After the run, outcomes are joined to decisions: was the resulting patch accepted by the gate, was the target obligation proven at the next ready revision, was the choice later overturned by a repair.

A wrong outcome is classified before anything is tuned: missing evidence in the state, a model error, a code error in enumeration or composition, or a service failure. Only a model error is a reason to reword a question or move a threshold.

The log is a run artifact. It is not persisted in the workspace store; the persistence format does not change.

## 13.3 What the log is for

- choosing thresholds from observed accuracy at each confidence, per question and per backend — offline, by replaying recorded probabilities against new thresholds. Nothing is re-asked while the evidence and the question meanings are unchanged;
- fitting the local backend's calibration temperature;
- measuring hosted–local agreement;
- finding questions whose answers do not predict outcomes, which are reworded or deleted.

---

# Part III — Execution

# 14. The executor seam

## 14.1 Current

`AgentBackend` is documented as mapping a task to a provider session: implementations "never interpret Conseqa semantics".

The test suite already contains the counterexample. `ScriptedBackend` is an in-process `AgentBackend` that holds a `ConfluenceEngine`, resolves the task token as a real MCP request would, and commits through the real engine, so that the OCC gate, analysis and invalidation behave normally.

## 14.2 Revised

```rust
pub struct SystemOneBackend {
    engine: ConfluenceEngine,
    decider: Arc<dyn Decider>,
    kinds: Vec<TaskKind>,                 // which task kinds are attempted in process
    policy: SystemOnePolicy,              // every builder's thresholds
    fallback: Arc<dyn AgentBackend>,      // the agent backend
}

impl AgentBackend for SystemOneBackend { ... }

pub enum Built {
    Committed { summary },
    NothingToDo { summary },
    Stale,
    Abstained(Abstention),                // reason, and what had been established
}
```

`run` passes a kind it is not enabled for straight to the fallback. Otherwise it resolves the task token, runs the builder for `invocation.kind` in process — raced against the task's cancellation — and either completes the session itself or, on an abstention, runs the fallback on the same task (§16). The scheduler, supervisor and workflow are unchanged.

It reports itself as `system_one+<fallback>`; a session it settles reports the backend `system_one`. The decision log is not the executor's concern: logging is a `Decider` layer (§11.5), so every builder is logged alike.

The `AgentBackend` contract is restated: an *external* backend never interprets Conseqa semantics; the in-process System One backend does, and is bound by every rule that binds an external agent.

## 14.3 What binds the executor

- **Capability.** It acts under the task's token and write scope. It holds no `WriteGrant` a session for that task would not hold.
- **Tracked reads.** Every architectural fact that influences a decision is obtained through the task-scoped engine API — `read_symbol`, `read_operation`, `graph_query`, `search_symbols`, `requirement_report`, `context_bundle`, `evaluate_candidate`. The read-set invariant (§110) holds without exception.
- **Run constants.** Run metadata — the prompt, the run policy — is fixed when the engine opens, and no mutation writes it. It is configuration, not an observed fact, and a builder may read it from the head. It is the only thing a builder reads there.
- **The gate.** It commits only through `submit`. Read-before-reference, draft validation, write scope, read-set validation and conflict detection apply unchanged.
- **Outcome.** The authoritative result is the engine's task state, as for any session.

---

# 15. Candidate evaluation

## 15.1 Need

A builder must learn whether a candidate patch validates, and whether it proves its target obligation, before choosing to submit it. The engine offers validation probes (`probe_model`) but no way to run the verifier over a hypothetical workspace.

## 15.2 Entry point

```rust
pub async fn evaluate_candidate(
    &self,
    task: TaskId,
    patch: &SpecPatch,
    roots: &[(Id, Id)],                        // (operation, transaction)
) -> Result<CandidateVerdict, EngineError>;

pub struct CandidateVerdict {
    pub scope_violation: Option<SymbolKey>,
    pub draft_diagnostics: Vec<DraftDiagnostic>,
    pub assembly_gaps: Vec<AssemblyGap>,
    pub validation: Vec<AnalysisDiagnostic>,
    pub verification: Option<VerificationReport>,
}
```

It applies the patch to a copy of the task's pinned workspace, then runs draft validation, `assemble_model`, `validate` and `verify` — pure functions — on a blocking worker. The fields are the stages a committed revision passes through, in order: a later one is meaningful only when every earlier one is empty.

It commits nothing, publishes no event, and invalidates no task.

The verdict carries the whole typed report rather than an answer about one obligation. What a caller asks of it is its own business, and two questions are provided:

- `standing(report)` — whether each declared requirement is proven, keyed by a `RequirementRef`: family, operation, transaction for a transaction family, and index into the declaration list. The identity holds between models that declare the same requirements, which is what a repair of a program body leaves alone;
- `verdict.regressions(baseline)` — the requirements proven in `baseline` and unproven under the candidate. One the candidate no longer declares is not among them: what is not declared is not unproven.

The **baseline is the empty candidate**. A builder does not read the head's analysis to learn what is unproven; it evaluates the empty patch, which runs the same pipeline against the same pinned snapshot and is observed the same way.

## 15.3 Observations

A verdict is a fact the builder relies on, so it is observed like any other. `roots` names the transactions whose obligations the caller will read off the verdict. Evaluation records, in the task's read-set:

- a symbol observation for the program, requirements, interface and runtime inputs of every operation in each root's conflict closure;
- for every data object those closures access or lock, the `Readers` and `Writers` query observations, so that a transaction that joins the closure afterwards is a phantom;
- the references of the patch, as a context bundle would.

The closure is taken over the *patched* model, because that is the model the verdict is about. With no root, the evaluation claims nothing about any closure and observes none.

## 15.4 Non-implications

- A candidate verdict is advisory. The gate validates against the head, and the analysis of the resulting revision is the authority. A verdict that went stale is rejected by the read-set; one that slips through is re-verified by the fixpoint.
- Evaluation is full verification over the assembled model. Its cost bounds the number of candidates (§18.4). Incremental verification is out of scope.

---

# 16. Abstention

A builder either commits a patch or abstains. It abstains when:

- the task kind, or the shape of this task, is outside what it enumerates;
- an answer it needs in order to act is uncertain. A preference among candidates the analyzer has already admitted is not such an answer: low confidence there leaves the deterministic order standing (§18.5). Nor is a judgment on a branch that is not taken;
- a requirement is stated and nothing enumerated can express it;
- no candidate validates, or no candidate proves;
- the decider is unavailable, or an engine read is refused;
- the gate refuses its patch for a reason other than staleness. The builder has no second idea, and the session is told what was tried.

Abstention happens before any commit. The same task — same token, same scope, the read-set accumulated so far — is then run by the fallback agent backend, whose prompt is extended with a hand-off: why the builder abstained and what it had established — the candidates it evaluated and their verdicts, a rejected patch — so the session does not retry dead ends.

Therefore:

- one task has at most one committer;
- a System One executor never commits partially and then abstains;
- the worst case of enabling the layer is the baseline plus the builder's milliseconds.

A submission the gate finds *stale* is handled as any stale submission is: the task is invalidated and the scheduler's attempt loop creates a successor, which re-derives from the new head. No warm-restart patch is inherited, because re-derivation is cheaper than reconciliation. A builder never resubmits.

A builder that is confident there is nothing to commit says so and ends its task, exactly as a session that found nothing to do would. That is an outcome, not an abstention.

---

# 17. Requirement discovery

## 17.1 Scope

All five families. A discovery task covers every family of its operation, and the workflow schedules it only while the operation declares nothing, so a builder that proposed part of the answer would leave the rest undiscovered for good. The builder therefore decides a task **whole or not at all**: it proposes everything it is sure of, or it abstains and proposes nothing.

It handles an operation with exactly one input. Anything else is outside what it enumerates.

## 17.2 Enumeration, in code

What a requirement is *keyed by* is a fact about the program, so code finds it.

- **The input key.** The declared identity of the triggering input: a request's `keyed` identity fields; for a subscription, the topic's message identity, when every schema the input admits maps the same fields — a key's path must resolve in every payload the input can carry. An outbox input declares none. This is the only key an idempotency or recoverability requirement can take.
- **Work.** Every inline transaction that mutates state — a write, insert, delete, transition, cursor advance, fence or outbox write — with a summary of what it reads and changes, in words a prompt could be matched against and never in DSL. A transaction that changes nothing has no committed history to constrain.
- **Serializability keys.** For each such transaction, the *input* values that pin an identity field of an object it accesses, through any step that selects one — reads and writes, and equally the locks and version guards that protect them. Only an input is available when the transaction begins, which a key must be.
- **Ordering guards.** Each `advance_cursor` and `fence` whose selector is pinned by an input and whose incoming position or token is an input, as the pair `(key, position)`.

A transaction with no candidate key yields no serializability candidate, and one with no guard no ordering candidate. One guard fixes both halves of an `OrderedBy`; several would need a judgment no question here makes, and the builder abstains.

Explicit obligations are found with the targeted search of §4.4, and only the unmapped ones are considered.

## 17.3 Questions

One request decides the task: everything code could not answer, asked together over one state — the prompt verbatim, the operation's description, trigger and work summaries, and the explicit obligations aimed at it.

- `obligation_<i>` — Choice, per explicit obligation: which enumerated requirement guarantees what it asks for. Each option describes a guarantee, not a DSL construct, and one option is an explicit no-match.
- `serializability_key_<t>` — Choice over a transaction's candidate keys, plus no-match; its instructions state the premise that this work must be serializable. Asked only where code found more than one candidate: with one there is nothing to judge.
- `idempotency`, `recoverability`, `serializability_<t>`, `ordering_<t>` — Noul: does the prompt state this requirement. Asked only under a policy that adopts implied requirements (§17.4) — and asked even where no key was enumerated, because a stated requirement code cannot express is the session's to handle, not a requirement to drop.
- `result_replay`, `guaranteed_completion` — Noul refinements, each stating its premise. Asked speculatively, because another question costs little and another request costs a round trip; read only where the requirement they refine is proposed.

A `stated` judgment falls in one of three bands: at or above `act` the requirement is stated; at or below `dismiss` it is not; between them the builder abstains. A chosen option is accepted at or above `select`. A refinement strengthens a requirement only at or above `act`, and otherwise the default stands: `unspecified` replay, `resumable` completion.

The thresholds are provisional — `act` 0.8, `dismiss` 0.25, `select` 0.6 — until they are chosen from the decision log (§13.3).

## 17.4 Provenance

| Condition | Result |
| --- | --- |
| an obligation's Choice selects an enumerated requirement at or above `select` | `ExplicitPrompt { obligation }` |
| it selects no-match, or nothing clearly | abstain: the obligation is real and must be mapped by someone |
| two obligations select the same requirement | abstain |
| the run's policy adopts implied requirements, and one is stated | `StronglyImplied`, citing the prompt as evidence |
| stated, and nothing enumerated can express it | abstain |
| uncertain whether stated | abstain |
| not stated | no proposal |

An explicit mapping settles a requirement: whether the prompt also implies it is then a branch not taken, and its uncertainty is ignored.

`rationale` is a fixed template naming the question and its probability. It is not generated prose.

The builder **proposes only what the run's policy adopts** — explicit mappings always, implied requirements only under `strict_requirements`, and never a `Recommended` one. A proposal that is merely recorded moves the head without changing what the operation declares, so the fixpoint would schedule the same discovery, and receive the same proposal, on every pass. Where the policy adopts only explicit requirements and no obligation targets the operation, the builder ends the task without asking anything.

Adoption itself is unchanged: `RunPolicy` decides what a proposal becomes, and a System One proposal has no privileged path.

## 17.5 What the tests establish

Against a model a person authored and the checker proves, with every declared requirement removed, and told only *that* the four requirements are stated, the builder re-derives the author's requirements exactly: idempotency and guaranteed recoverability keyed by an identity the subscription inherits from its topic, `SerializableBy` keyed through the transaction's selectors, and `OrderedBy` read off its cursor. The remaining judgments — is it stated — are the only ones a model makes.

---

# 18. Requirement repair is generate-and-verify

## 18.1 Principle

An unproven serializability obligation names its unconstrained dependencies and, for each, its gaps: the transaction, the side, the object, the step. Each gap admits a small set of mechanical edits. Code synthesizes them, the analyzer judges every one, and System One is asked only which *proven* candidate suits the application.

A remedy therefore does not have to be right. One that does not prove its target is simply not admissible. What a remedy must never do is edit outside the program it was given.

## 18.2 Remedy catalogue

A remedy is a pure function of the operation's program, the obstacles, and which objects carry a version:

```rust
fn candidates(operation, program, obstacles, versions) -> Vec<Remedy>

struct Remedy { kind, program, transactions_edited, steps_added, summary }
```

| Route | From | Edit |
| --- | --- | --- |
| declared isolation | `IsolationUnspecified` | declare `read_committed`, the weakest declaration that says a transaction reads committed data and installs conflicting writes in commit order |
| serializable closure | `SerializableClosureContainsWeakerIsolation` | declare `serializable` on every weaker member. A candidate only when every weaker member is this operation's: the route needs all of them |
| strict locks | `LockCoverageMissing`, `LockAcquiredAfterProtectedAccess` | take an exclusive lock on the accessed selector at the head of the transaction; move a covering lock that is acquired too late rather than duplicating it. Exclusive on both sides — whether a weaker mode would have done is the analyzer's to say |
| version protocol | `VersionValidationMissing`, `VersionBumpMissing` | select the version field in the read of the instance and add `validate_version` directly after it, reusing the read's selector so that the guard identifies the very instance observed; add `bump_version` after the last step that mutates the object |

The version protocol carries one qualification. A guard *rejects*, and what an operation does when its transaction is rejected is its author's decision, not a mechanical edit. The route is a candidate only where the transaction already declares a `rejected` arm: the author has then said what a rejection does, and the regression check of §18.4 holds that arm to every other declared requirement. Where there is no arm, the route is not offered, and the repair escalates.

A remedy returns nothing when inapplicable, and one that would change nothing is not a candidate. A remedy that would edit a symbol outside the task's scope — declaring a `version` on a shared data object, for instance — is not a candidate either.

Ordering obstacles are not in the first catalogue, and nor is any operation family. They escalate.

## 18.3 Scope

The first increment repairs within the workflow's existing repair task: one operation, one program. A closure that spans operations is then repaired only by a route that needs no edit elsewhere — the version guard of `flash_checkout`'s `apply_payment` is one — and otherwise escalates, with what was tried.

A remedy may need several templates: a strict-lock proof needs the lock on the reader and on the writer. A later increment creates a System One repair task over the obligation's conflict closure, with an `OperationProgram` grant for each operation a remedy may edit. The scheduler footprints it accordingly; it shares no wave with a task touching those programs.

## 18.4 Evaluation

The builder evaluates the empty candidate first (§15.2). With nothing unproven for its operation, it says so and ends the task. Its targets are the operation's unproven serializability requirements; whatever else is unproven is named in the hand-off or the commit summary, and left.

Candidates are evaluated concurrently through `evaluate_candidate`, bounded by `max_candidates`, with every serializability-constrained transaction of the program as a root.

A candidate is *admissible* when it validates, every target is declared and proven under it, and it has no regression anywhere in the model.

Unlike discovery (§17.1), repair need not decide its task whole. The workflow re-enumerates what is unproven on every pass, so an obligation the builder leaves is offered again — to the builder, which abstains, and so to a session — and the regression check is what makes a partial repair safe. The cost is a pass of the fixpoint.

## 18.5 Preference

Absent evidence, preference is deterministic: *least invasive first* — fewest transactions edited, then fewest steps added, then catalogue order.

System One may reorder admissible candidates only on a stated domain fact, and an unstated fact reorders nothing. The first increment knows one: **heavy simultaneous demand for one record**. Under serializable isolation a conflicting execution is aborted and must be retried; under an exclusive lock it waits its turn. So when both routes are admissible and isolation leads, one Noul is asked — does the prompt state such demand — and at or above `act` the locks are submitted instead.

The question is asked only when its answer could change what is submitted. One admissible candidate means nothing is asked at all, and repair then runs without a decider.

A preference is not a judgment the builder needs in order to act (§16). An unsure decider, or an unavailable one, leaves the deterministic order standing, and the repair is committed all the same.

With no admissible candidate, the builder abstains and passes every evaluated verdict to the agent session.

## 18.6 Non-implication

System One never makes an obligation proven. It chooses among specifications the analyzer has already proven. A wrong choice is a defensible design, not an unsound one.

## 18.7 What the tests establish

Against `tenant_ledger`, a model a person authored and the checker proves, the transaction that serializes postings is broken three ways — its lock removed, its lock taken after the read it protects, its isolation left undeclared — and each time the builder arrives back at the author's program: by the lock route when the prompt states contention, by declared isolation when that is all that is missing. With nothing stated and the lock removed, it declares serializable isolation instead, which the analyzer proves as well.

Against `flash_checkout`, whose `apply_payment` conflicts with two other operations, removing the version guard leaves two candidates. The analyzer admits one, and it is the author's program again. With the `rejected` arm removed as well, the version route is not offered, the lock route does not prove, nothing is committed, and the session is told both.

---

# 19. Runtime topology

L1 proves no transaction property, and only the replay families consume its delivery facts. Its authoring is the safest to mechanize.

## 19.1 Defaults, in code

Derived from L0 alone:

- one execution pool per service;
- one router per request and subscription boundary, routed by the input's declared identity where one exists;
- one storage layout per data model;
- a runtime for every topic, subscription and outbox, grouped by the declared message identity.

## 19.2 Knobs

Delivery semantics, member assignment, member concurrency, transport ordering and batching are closed sets. Each is a `stated` Noul followed by a Choice. Unstated, the conservative default stands.

## 19.3 Conservative defaults

A default never asserts a stronger fact than the prompt supports. Delivery defaults to `at_least_once`. A replay obligation must not become provable because a default invented a delivery guarantee.

L1 structural validation guards the result. Diagnostics the builder cannot resolve cause abstention.

---

# 20. Operation synthesis by archetype

Later phase. The mechanism is fixed here; the catalogue is measured, not assumed.

## 20.1 Archetype

An archetype is a typed program template with:

- an applicability question;
- slots, each with an enumerator over the symbol graph — which object, which state machine and transition, which topic or outbox, which input field is the key;
- a deterministic identifier scheme for the ids it introduces (`tx.<operation>.<role>`, `read.<operation>.<object>`).

## 20.2 Seed catalogue

Drawn from the fixtures: keyed insert; read–validate–write under a version; state transition with a transition-scoped outbox admission; external effect with result matching and a compensating arm; outbox consumer with dispatch; ordered subscription consumer under a successor cursor.

## 20.3 Selection

Rank wide, then re-rank narrow. One request ranks every archetype against the operation's interface and prompt evidence and asks, separately, whether any archetype applies at all. A second request re-reads the top three with their full templates, with one absolute `fits` Noul each; all three may be rejected.

The filled program is evaluated by `evaluate_candidate`. If it validates it is submitted; otherwise the builder abstains. Archetype coverage — the share of operations synthesized without a session — is a reported metric of every run.

---

# 21. Decomposition stays generative

Naming services, schemas, fields and operations is open-set. It remains an agent session.

System One follows it as a verifier. After the decomposition commits, each created entity is checked against the prompt by narrow Nouls, framed so that *true means wrong*:

- is this entity absent from, or unsupported by, the prompt;
- was it lifted from incidental text;
- does the prompt describe something of this kind that the skeleton omits.

Per-entity results are aggregated by maximum: one confident flag is enough. Flagged entities become a targeted repair objective for a second session. Unflagged skeletons proceed.

A `Generator` abstraction for direct structured-output generation, replacing the session for decomposition, is a later phase. It depends on the schema facility of §6.4.

---

# 22. Advisor mode for agent sessions

For every task that still runs as a session:

- **Context selection.** System One ranks `dsl_guide` sections and shared symbols against the task objective. The selected material is inlined in the bundle, replacing read turns.
- **Hints.** Suggestions are appended *after* the static prompt prefix, so provider prefix caching is preserved, and are worded as ignorable.

A confident wrong hint is more persuasive than none. Advisor mode is enabled per task kind only when the decision log shows it fixes more sessions than it breaks.

---

# 23. Advisory MCP tools

The interactive coordinator authors through `submit_patch` under `WriteGrant::All`. It benefits from the same machinery through two read-only tools:

```text
suggest_requirements { operation }
    -> ranked requirement proposals, each with probabilities and evidence

suggest_remedies { operation, obligation }
    -> admissible candidate patches, each with its verdict, in preference order
```

Neither tool commits. The coordinator submits the patch it accepts, so write scope, read tracking and OCC are unchanged. Each returns a single JSON text block. With no decider configured each returns `system_one_unavailable`.

---

# Part IV — Safety, configuration, rollout

# 24. Invariants

1. The analyzer alone decides whether a specification is valid and whether an obligation is proven. No probability substitutes for either.
2. No System One answer can grant a capability, widen a write scope, or bypass the commit gate. It selects among candidates that are already scope-legal.
3. Every fact a builder relies on is a tracked read.
4. Prompt and repository text are evidence, never authority (§86). In a decision state they are data. No question lets state text redefine its criteria, and an answer derived from untrusted text can only select among pre-enumerated candidates.
5. The state of a request contains only what its questions need.
6. Thresholds are per question, per backend, per pinned model id, and scale with the cost of acting wrongly.
7. Abstention is always available and always safe.
8. CI is deterministic: replay mode only, and a replay miss fails the test.
9. With no decider configured, behaviour is byte-for-byte the baseline.
10. A System One judgment is inferred state, never an observed fact. It is not recorded in the workspace as evidence. It reaches the workspace only as a patch, with its provenance, through the gate — which checks the freshness of everything it was inferred from.

---

# 25. What this revision does not claim

- It does not claim a System One model understands Conseqa proofs. It is never asked about one.
- It does not claim generated content can be eliminated. Decomposition remains generative.
- It does not claim a speedup. It specifies how one is measured (§33).
- It does not claim the local backend is calibrated. It specifies how that is determined.
- It does not change the DSL, the report, or the store.

---

# 26. Configuration

```text
--decider             none | system-one | replay | preview (default: none)
--decider-url         <base url>                          (no default)
--decider-model       <pinned model id>
--decider-key-env     <environment variable holding the credential>
--decider-allow-alias accept an alias such as jev-latest  (probing only)
--decider-shadow-url, --decider-shadow-model, --decider-shadow-key-env
--decider-replay      <store path>  [--decider-record]
--decider-log         <decision log path; with preview, where the requests are written>
--system-one-kinds   <task kinds, comma separated>        (default: none)
--system-one-config  <thresholds and limits, TOML>         (not yet built)
```

The executor is never enabled implicitly: it needs both a decider and the kinds it may attempt. `--system-one-kinds` accepts only a kind that has a builder — naming another would silently do nothing — and is refused without a decider. The settings are validated before the database is opened or the MCP endpoint served, so a run never starts, and nothing is sent, on settings that are wrong.

The hosted service is `--decider system-one --decider-url https://api.typesafe.ai --decider-key-env TYPESAFE_API_KEY`; a local server is the same decider at a loopback URL with no credential. Neither is ever selected implicitly. `conseqa-harness design` and the daemon's three serving modes — whose `request_design` runs the same workflow — accept the same settings, from one definition (`harness::executors::cli`). A daemon validates them at startup and wraps its agent backend once per design run, because a builder acts on one engine and each run has its own project's. A URL that is not loopback is announced on standard error before anything is sent, together with what a run sends there: its prompt, and a summary of each operation a builder decides.

Two commands exercise a decider without a run, and send only synthetic material:

```text
conseqa-harness decider probe        one small request; prints the typed answers, latency and usage
conseqa-harness decider conformance  the admissibility suite of §32; exits 2 when a probe fails
```

---

# 27. Crate layout and features

```text
src/system_one/
    mod.rs          Decider, Question, Answer, DecisionRequest
    config.rs       DeciderSettings: which backend answers is configuration
    http.rs         SystemOneHttpDecider (hosted or local, by base url)
    conformance.rs  the admissibility suite for a local server
    logprob.rs      LogprobDecider (optional)
    replay.rs       ReplayDecider
    preview.rs      PreviewDecider
    shadow.rs       ShadowDecider
    log.rs          DecisionLog
    questions/      every QuestionSpec, and nothing else

src/harness/executors/
    mod.rs          SystemOneBackend, Built, Abstention, fallback
    cli.rs          the flags of §26, shared by every entry point
    describe.rs     transactions in words, for a decider's state
    discovery.rs    requirement discovery builder
    repair.rs       generate-and-verify repair builder
    remedies.rs     the remedy catalogue
    topology.rs     runtime topology builder
    archetypes/     operation synthesis, later phase
```

```toml
system-one = ["confluence", "dep:reqwest"]
```

`reqwest` becomes an optional dependency of this feature. The feature is on by default, so the harness CLI carries the `decider` commands; `--no-default-features --features confluence` and the lean checker still build without an HTTP client.

---

# 28. Files directly affected

Contract refresh:

```text
src/confluence/mcp.rs            family doc, DSL_REFERENCE, INTERACTIVE_INSTRUCTIONS, dsl_schema
src/confluence/engine.rs         unknown family error
src/confluence/patch.rs          conflict footprint
src/confluence/read_set.rs       SearchSpec.targets
src/harness/scheduler.rs         footprint, task ledger
src/confluence/task.rs           stale doc
src/confluence/commit.rs         stale doc
src/harness/supervisor.rs        budget enforcement
src/bin/harness/main.rs          default budget, decider flags
src/bin/confluence/main.rs       decider flags
src/spec/**                      JsonSchema derives, confluence feature only
CONSEQA_AGENT_CONFLUENCE_HARNESS_IMPLEMENTATION_SPEC.md
```

System One layer:

```text
src/system_one/**                new
src/harness/executors/**         new
src/confluence/candidate.rs      new: CandidateVerdict, standing, regressions
src/confluence/engine.rs         evaluate_candidate
src/confluence/mcp.rs            suggest_requirements, suggest_remedies
src/harness/backend.rs           AgentExit.escalation
src/harness/workflow.rs          manifest format 2, closure-scoped repair tasks
src/harness/task_prompt.rs       targeted obligation search, advisor hints
Cargo.toml                       system-one feature
```

---

# 29. Versioning

`DSL_VERSION`, report `FORMAT` and persistence `FORMAT` are unchanged.

`confluence-manifest.json` gains `manifest_format: 2`. Its additions are additive.

The MCP surface gains `dsl_schema`, `suggest_requirements` and `suggest_remedies`, and changes one behaviour: an unknown `requirement_report` family is an error.

---

# 30. Superseded documentation

Harness specification §74.1 and §74.2 are replaced by §7 of this document. §40 is corrected to the v4 summary fields. §44 and §50 document the family values and the new tools. §55 is restated per §14.2. §87 gains the decision log. §88 gains the configuration of §26.

---

# 31. Minimum implementation sequence

Each phase ends with the benchmark of §33. A phase that lowers proven-over-total or prompt-obligation mapping is not enabled by default.

0. **Contract refresh.** Part I entire. No new dependency.
1. **Telemetry and baseline.** Manifest format 2; the benchmark corpus; an agent-only baseline recorded under `reports/`.
2. **Primitives.** `Decider`, the wire-format, replay and shadow backends, the conformance suite, the question registry, the decision log, replay fixtures. Exit: a local server that passes conformance, and a hosted–local agreement report over a fixed question set.
3. **Executor seam and discovery.** `SystemOneBackend`, abstention, requirement discovery for all five families, the targeted obligation search (§4.4), and the task records of §13.1. `evaluate_candidate` moves to phase 4, its first user.
4. **Repair.** The conflict footprint of §4, which repair's eagerness depends on; `evaluate_candidate`; the remedy catalogue; generate-and-verify within the per-operation repair task. Closure-scoped repair tasks (§18.3) follow.
5. **Topology.** Defaults and knobs.
6. **Advisors.** Advisor mode; `suggest_requirements`; `suggest_remedies`.
7. **Archetypes.** Operation synthesis, with coverage reported.
8. **Generation.** The `Generator` abstraction and the decomposition cascade.

---

# 32. Minimum test matrix

## Contract refresh

- an unknown `requirement_report` family is an error naming the accepted values; every documented value filters;
- a transaction-family `propose_requirements` conflicts with a concurrent `replace_operation_program` of the same operation, at the gate and in wave planning;
- a requirement-discovery scope still cannot replace a program;
- a search for the obligations targeting one operation returns only those; rewriting an obligation aimed elsewhere leaves the searcher valid, and aiming one at its operation invalidates it;
- every example in `dsl_reference` parses into its type, and every program and transaction example passes `program_local_diagnostics`;
- the reference contains a JSON shape for every `Mutation`, `OperationStep` and `TransactionStep` variant, established by round-tripping the typed examples, not by searching the text;
- `dsl_schema` returns a schema that accepts the corresponding reference example;
- the handshake instructions promise no `runtime` remedy;
- a session exceeding a declared token or cost budget is cancelled and recorded.

## Primitives

- each backend maps a recorded provider response to the typed answers;
- replay answers from the store; a miss is an error; a reworded question misses;
- shadow mode drives behaviour from the primary only and logs both;
- an oversized request is refused, not truncated;
- a wide `Choice` on the `LogprobDecider` is evaluated as a tournament and marked degraded.

## Local server conformance

Black-box, against a running server. Each probe fails a server that reproduces the wire shape without the behaviour:

- *criteria are read*: option keys are opaque ids, distinguishable only by their criteria, and each message is asked under two rotations that move every description to another key and another position. The right *description* must be chosen every time, which also rules out a preference for a key or a position;
- *every option is offered*: a thirty-option `Choice` whose correct option sorts last is answered correctly;
- *probabilities are the model's*: three choices the state says nothing about, offering four, five and six options. A rule-assigned floor gives all three the same winner over an exactly uniform remainder, whatever the option count; a model does not — not even one that is confidently wrong and rounds its numbers. Uniform uncertainty passes. Answering all three at 0.9 or above is reported as a warning: the numbers are the model's, but they do not express its uncertainty;
- *the unaskable is an error*: requests the hosted service refuses — no state, an unknown question type — are refused (a one-level `Score` is not among them: the reference says it fails validation, but jev-1.13.0 answers it). A server that answers what it cannot resolve will also answer when it has merely failed;
- *questions are isolated*: adding an unrelated question to a request moves no other answer beyond tolerance;
- *typed shapes*: a plainly true `Noul`, a plainly false one, and a `Score` at the top of its rubric are each answered sensibly in their typed shape;
- *answers are repeatable*: asking again moves no answer beyond tolerance, or a warning says recordings of this server will not reproduce it.

Every answer is also checked for conformance by the decider itself (§10), so an unreported option, a distribution that is not one, or a choice that is not the most probable fails whichever probe met it.

The suite is itself tested against a scripted server with one mode per misbehaviour — flooring, answering the unaskable, showing only twenty options, ignoring criteria, leaking sibling questions — and each mode must fail exactly the probes written for it. The hosted service is the reference: a probe it fails is a wrong probe.

## Executor

- a System One task acts under its task token, and an out-of-scope candidate is a scope violation, not a commit;
- every read a builder makes appears in the task's read-set;
- `evaluate_candidate` commits nothing, publishes nothing a commit would, and invalidates nothing; a candidate outside the scope is judged no further; what it evaluated still commits through the gate;
- a task whose rooted closure gained a conflicting transaction after evaluation is invalidated as a phantom, and one that named no root is not;
- a candidate that un-proves a requirement reports the regression;
- abstention runs the fallback on the same task, which has exactly one committer, and the session's prompt carries the hand-off;
- an unavailable decider costs a run only the fallback;
- the manifest records who settled each task, and an abstention against the task the agent settled;
- with no decider configured, the workflow suite is unchanged.

## Builders

- discovery maps an explicit obligation to the enumerated requirement without an agent session; one it cannot place — no match, or no clear match — goes to the agent backend;
- discovery maps the bands to `StronglyImplied`, abstention, and no proposal; a prompt that states nothing needs no session; a refinement that is not stated leaves the default;
- a stated requirement nothing enumerated can express is escalated, never dropped;
- on `tenant_ledger` with every declared requirement removed, enumeration re-derives what its author declared, for a request and for a subscription whose identity comes from its topic;
- discovery tasks that overlap, each mapping its own obligation, each commit at the first attempt;
- on `flash_checkout`, removing `apply_payment`'s version validation yields two candidates, one admissible, and the author's program;
- on `tenant_ledger`, each of three breakages of the posting transaction is repaired back to the author's program, by the route the stated facts prefer;
- an unsure or unavailable decider does not stop a repair; one admissible candidate asks nothing;
- an operation with nothing unproven needs no repair and no session;
- an obligation for which no catalogue remedy is admissible abstains, and the session it falls back to receives every evaluated verdict; without a `rejected` arm the version route is not offered;
- a candidate that proves its target but un-proves another obligation is not admissible;
- a remedy needing a `version` on a shared object files a dependency request;
- no remedy emits a `validate_version` whose selector does not pin identity;
- preference is least-invasive-first when nothing is stated, and reorders only on a stated fact;
- topology defaults assert no delivery guarantee the prompt does not state.

---

# 33. Measurement protocol

## 33.1 Corpus

A fixed set of natural-language prompts, one per fixture system — `flash_checkout`, `tenant_ledger`, `video_streaming`, `payment_capture`, `transactional_outbox` — each with its expected properties: the obligations that should be declared, those that should prove, and the prompt obligations that should map.

## 33.2 Arms

```text
baseline      agent sessions only
treatment     System One enabled for the phase's task kinds, agent fallback
```

each with the hosted backend primary and the local backend in shadow, and the reverse.

## 33.3 Metrics

Speed: wall time to terminal status; sessions spawned; turns; LLM tokens; System One requests, tokens, cost and end-to-end request latency.  
Quality: obligations proven over total; prompt obligations mapped; rejected submissions; repair iterations.  
System One: abstention rate by reason; escalation rate; accuracy at each confidence band per question; selective risk against coverage — the error rate among decisions acted on, as the act threshold moves; expected calibration error; hosted–local agreement; archetype coverage.

A backend's confidence is not assumed to flag its errors. Whether it does is what selective risk measures, per backend, on Conseqa's own questions. Agreement on public benchmarks predicts nothing about them.

A result is reported as a distribution over repeated runs, never one run.

---

# 34. Risks

- **Literal questions.** A question is answered as written. Mitigated by review of `questions/` as data, and by deleting questions the log shows to be uninformative.
- **Hosted availability.** Rate limits are declared to change without notice. Mitigated by the circuit breaker and abstention.
- **Data egress.** Mitigated by explicit opt-in and by the local backend.
- **Uncalibrated local confidence.** A stock model's option probabilities are not calibrated, and its confidence need not flag its errors. Mitigated by separate thresholds, the shadow comparison, selective-risk measurement, and fitted calibration.
- **An immature ecosystem.** Open System One servers and scorers are days old at the time of writing, and their cards overstate: a repository named for a training method may contain no trained weights, and an engine advertising calibrated confidence may floor it. Mitigated by the conformance suite, pinned revisions, weights from identifiable publishers in a non-executable format, and never running an unreviewed server.
- **Persuasive wrong hints.** Mitigated by gating advisor mode on measured fix-versus-break counts.
- **Cost of candidate evaluation.** Full verification per candidate. Mitigated by `max_candidates` and least-invasive ordering; incremental verification remains future work.

---

# 35. Open items

1. Whether a replay-family obligation whose only missing premise is a delivery fact should produce `RemedyLayer::Runtime`.
2. Enumerating the candidate space of idempotency, result-replay and recoverability discovery.
3. Remedies for ordering obstacles, several of which require a managed field on a shared object.
4. Whether `evaluate_candidate` warrants closure-scoped incremental verification.
5. The direct generation client for §21, and its credential handling.
6. Which local model serves in shadow: a trained open scorer, with a stock instruct model of the same size as the control. Decided by the shadow comparison on Conseqa's own questions.
7. Whether Conseqa ships a reference local server for the wire format, or only the conformance suite.

---

# 36. Final architecture

```text
prompt
  |
  v
decompose ............ agent session           -> System One fidelity check -> targeted repair
  |
  v
operation synthesis .. archetype builder       -> evaluate_candidate -> submit | abstain -> session
  |
  v
requirement discovery  enumerate x System One  -> propose_requirements      | abstain -> session
  |
  v
analysis ............. the analyzer            (authority)
  |
  v
repair ............... remedy catalogue -> evaluate_candidate -> System One preference
  |                                                          -> submit | abstain -> session
  v
runtime topology ..... defaults + stated knobs -> submit | abstain -> session
  |
  v
finalize
```

Every arrow into the workspace passes the same gate: capability, write scope, read-before-reference, draft validation, read-set validation, conflict detection.

Every decision is logged with its probabilities, answered by two backends, and joined to its outcome.

> Code owns control flow. A System One model chooses among options code has enumerated. A generative model writes what cannot be enumerated. The analyzer alone decides whether a specification is correct.
