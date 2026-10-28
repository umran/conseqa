# Conseqa Spec Change Analysis Revision Specification

## 1. Status

**Status:** Proposed  
**Part:** I of VI: semantic diff and obligation diff. Parts II, III and V are outlined in §21 and are not normative. Part IV (conformance) and Part VI (realization profiles) are specified in their own documents.  
**Target DSL version:** 6 (unchanged)  
**Project store format:** 10 (unchanged)  
**Obligation report format:** 9 → 10  
**Fact format:** 1 (new, §10.9)  
**Baseline:** Conseqa `master`, commit `00c029f64738157c33e65d8e1cb41121c38ca831`  
**Primary affected subsystems:** the obligation report, the project store reader, the `conseqa` CLI, the design MCP server

This revision makes a **change to a Conseqa spec** a first-class object. Given two spec trees, typically two git revisions of one repository's `.conseqa/`, Conseqa SHALL report:

1. **what changed in the architecture**: the declarations added, removed, or modified, each attributed to exactly one declaration;
2. **what changed in what the architecture proves**: which obligations were added, removed, regressed, improved, held, or held on a different argument;
3. **why**: for every changed verdict, the changed declarations that explain it;
4. **what the implementation must now provide**: the declared facts newly relied on, and no longer relied on, by any proof.

The revision introduces:

- the **declaration partition**, an attribution of every semantic fact of a workspace to exactly one declaration;
- the **obligation key**, a position-free identity for an obligation that survives reordering;
- the **basis**, the structured, position-free set of facts a proof consumed, with a durable and versioned form for facts stored outside the run that computed them;
- the **obligation diff**, the **explanation invariant**, and the **reliance delta**;
- `conseqa diff`, the design MCP tool `diff_spec`, and obligation report format 10.

It changes no DSL semantics and gives the model's `revision` no meaning.

---

# 2. Motivation

## 2.1 Git versions the text, not the architecture

A Conseqa project lives in its repository, and git is its history (`PROJECT_STATE.md`). The engine numbers its commits per machine, and the model's `revision` is opaque: §2 of the semantics contract forbids reading revision `2` as compatible with, derived from, or newer than revision `1` "unless surrounding tooling establishes that convention". So git branches, merges, and reviews a spec like any other source, and that part needs no new mechanism.

A text diff of `.conseqa/`, though, answers only which YAML lines changed. It does not say:

- which declarations changed, as opposed to which files were touched;
- whether a change is semantic at all, since a sketch, a stage, or a description carries no semantic weight;
- which obligations the change affected, or why;
- what the implementation must do differently for the proofs to keep holding.

## 2.2 A proof can stay proven while its meaning changes

Every proof is conditional on the implementation conforming to the declarations it cites (§1.3, §25). Consider a change that replaces

```yaml
isolation: serializable
```

on `tx.apply_payment` with a read-committed transaction whose update becomes a `compare_and_set` of the order's version against the version its read observed.

`conseqa check --require-proven` passes before and after. The `SerializableBy(order_id)` obligation is proven on both sides. But the argument changed: it rested on the database running the transaction serializably, and now rests on the implementation issuing one conditional update. The implementation obligation changed completely, and nothing in today's output says so.

## 2.3 Dropping a requirement is silent

The cheapest way to make `conseqa check --require-proven` pass is to delete the requirement it fails on. Today that change is indistinguishable, in every Conseqa output, from never having declared the requirement.

## 2.4 What blocks a diff today

The pieces mostly exist. The design layer builds a symbol graph whose every symbol carries a semantic fingerprint (`src/design/symbol.rs`, `src/design/graph_build.rs`), and the verifiers return structured proofs (`src/analyzer/verification/`). Two things stand in the way.

**Obligation ids are positional.** An obligation's id embeds the index of its requirement in its declaring list: `oblig.<operation>.<family>.<index>` and `oblig.<operation>.<transaction>.<family>.<index>` (`src/analyzer/report.rs`, rebuilt independently in `src/design/engine.rs`, `src/harness/workflow.rs`, `src/viz/transaction_proofs.rs`, and `viz/src/lib/transactionProofs.ts`). Reordering two requirements renames both obligations.

**Proof assumptions are prose.** `Obligation.assumptions` is a `Vec<String>` rendered from the structured proof. Its wording is free to change between builds, it embeds step positions, and it cannot be indexed by declaration. The question "which proofs rest on this declaration?" cannot be answered from it.

The symbol graph's fingerprints are also the wrong tool for attribution, for a reason §7.1 explains.

## 2.5 Why this comes first

The questions that matter for a deployed system all start from this revision:

- data-model migrations (Part III) need to know exactly which declarations changed;
- realization bindings to code and infrastructure (Part IV) need the reliance delta;
- mixed-version verification (Part V) needs both sides' bases.

§21 says how.

---

# 3. Goals

## 3.1 The change is the unit

The subject of analysis is an ordered pair of spec trees, **base** and **head**, not a single model and not a revision number.

## 3.2 Every change has exactly one owner

A semantic fact of a workspace belongs to exactly one declaration. A change to it is reported once, at that declaration, not also at every aggregate that contains it.

## 3.3 Obligations have position-free identity

An obligation keeps its identity when its requirement moves within its list, when unrelated requirements are added beside it, and when the steps of its transaction are renumbered.

## 3.4 A verdict and its argument are compared separately

"Proven on both sides" splits into **held** (on the same basis) and **rebased** (on a different one). A rebased obligation is a change to what the implementation must guarantee, and is reported as one.

## 3.5 Every verdict change is explained

Every changed status, basis, or set of obstacles is attributed to at least one changed declaration. An unexplained change is an analyzer defect, reported as such and never dropped.

## 3.6 Conformance changes are visible model-wide

The diff states which declared facts the implementation must newly guarantee and which no proof needs any longer.

## 3.7 Usable by CI and by agents

The project CLI can gate a pull request on the diff, and a design agent can ask the design MCP server what its work changed relative to a base.

---

# 4. Non-goals

This revision does not:

- change the DSL, its validation, or any verdict;
- give `revision`, the store `FORMAT`, or the crate version any ordering or compatibility meaning;
- infer renames or moves: an id is a declaration's identity, so a renamed declaration is a removal and an addition (declared moves are Part II);
- compare a model with an archived obligation report, or diff across a DSL or store format bump;
- model deployments, environments, in-flight state, migrations, code or infrastructure bindings, or mixed-version execution (Parts II–V);
- add a visualization overlay for diffs;
- provide a three-way obligation diff for merges, or a semantic merge tool: git merges the text, and `conseqa check` judges the merged result.

---

# 5. Terms

| Term | Meaning |
|---|---|
| **Spec tree** | The committed content of one `.conseqa/` directory: everything except `local/`. |
| **Side** | One of the two spec trees of a diff: **base** or **head**. |
| **Declaration** | A member of the declaration partition (§7): the unit to which semantic changes are attributed. |
| **Residual** | The content a declaration owns: its canonical form minus the content owned by the declarations it contains. |
| **Obligation key** | The position-free identity of an obligation (§9). |
| **Terms** | The parts of a requirement that do not identify it, and whose change restates it (§9.3). |
| **Basis** | The set of basis facts a proven verdict consumed (§10). |
| **Basis fact** | One declared fact, or one model-wide query result, stated without positions and anchored to the declarations its truth was read from. |
| **Blocked-on set** | The basis facts, missing facts, and anchors an unproven verdict's obstacles cite (§10.7). |
| **Reliance** | The union of the bases of every proven obligation on one side (§13). |

---

# 6. Reading two spec trees

## 6.1 Sources

Each side is read from exactly one of:

- **a git commit**, named by any expression `git rev-parse` resolves to a commit (a branch, a tag, `HEAD~2`, a hash);
- **the working tree**: the project's files as they are on disk, uncommitted edits included.

## 6.2 One loader for every source

Both sides SHALL be loaded by the same loader the store uses to open a project, `load_workspace` in `src/design/store.rs`. That loader SHALL be factored over a file source, a directory or a git tree, so that every refusal applies identically to both sides:

- a `project.yaml` of another `format` or `dsl` is refused by name;
- an unexpected directory or entry under `spec/` or `spec/runtime/` is refused by name;
- a file name that is not its id's canonical encoding is refused;
- invalid YAML or an unknown field is refused with the file and the error.

A git tree is read with read-only git plumbing (`rev-parse`, `ls-tree`, `cat-file`) as a subprocess. Reading a side SHALL NOT check out a commit, create a worktree, touch the index, write any file, or take the project's writer lock. `local/` is never read from any source; git does not hold it in any case.

## 6.3 Locating the project in a tree

The project is discovered from the working directory exactly as `conseqa status` discovers it. Its path relative to the repository root (`git rev-parse --show-toplevel`) is the path read at every git side. Outside a git repository there is no git side to read, so a diff naming one is refused by name.

## 6.4 An absent project

A git side whose tree holds no `.conseqa/project.yaml` at the project's path is read as the **empty project**, and the diff records that the side was empty. Diffing a branch that introduces a project against one that predates it therefore reports every declaration as added. A ref that does not resolve is an error, never an empty side.

## 6.5 Version axes

Both sides SHALL be of the building binary's store `FORMAT` and `DSL_VERSION`. A side of another version is refused by name, naming the side and both versions. A contract bump changes what programs mean (the DSL 6 precedent), so a diff across one would compare texts that do not share a semantics; bringing a design across a bump is re-authoring, not a diff.

Both sides are analyzed by **one build**. The diff therefore measures the change in the model alone, never a change in the analyzer. This is also why an archived report is never a side: the same text can prove differently across builds, which is why `ProverReport` records its `dsl`.

## 6.6 Determinism

For two given trees, the diff document (§14) SHALL be byte-identical across runs and machines. Every list in it has a defined order.

---

# 7. The declaration partition

## 7.1 Why not the symbol graph's fingerprints

The symbol graph's fingerprints are **observation** fingerprints. They overlap on purpose: a task that read an operation's program observed everything in it, so the program's fingerprint covers its transactions; the transaction's covers its requirements; and `SymbolKey::Operation` fingerprints the whole draft, including its sketch, its stage, and its description. That overlap is exactly right for invalidation (`src/design/invalidation.rs`), where a stale observation must be caught whatever part of it moved.

It is wrong for attribution. One edited lock step would be reported as a change to the transaction, the program, and the operation; a sketch-only edit would be reported as a change to the operation. The diff therefore uses its own projection, the **declaration partition**, in which every semantic fact has exactly one owner. The invalidation fingerprints are unchanged.

## 7.2 Partition members

| Layer | Declaration | Residual (owned content) |
|---|---|---|
| L0 | `service` | The service declaration. |
| L0 | `schema` | The schema minus its `description`. |
| L0 | `data_model` | The data model minus its objects and outboxes. |
| L0 | `data_object` (data model, object) | The object: schema, identity, version, managed fields. |
| L0 | `outbox` (data model, outbox) | The outbox: messages, message identity. |
| L0 | `topic` | The topic: messages, message identity. |
| L0 | `state_machine` | The machine minus its transitions. |
| L0 | `transition` (machine, transition) | The transition, side effects included. |
| L0 | `operation` | The operation's `service`. |
| L0 | `input` (operation, input) | The input. |
| L0 | `program` (operation) | The program's control structure. Each inline transaction is replaced by its id and each effect contract by its effect id. Blocks reached from a transaction step, its rejected arm included, are control structure and belong here. |
| L0 | `transaction` (operation, transaction) | The transaction declaration minus its requirements. |
| L0 | `effect_site` (operation, effect) | The effect contract at the site, outbox writes included. |
| L0 | `requirement` (requirement identity, §9.2) | The requirement's terms (§9.3). |
| L1 | `topic_runtime`, `subscription_runtime`, `outbox_runtime`, `execution_pool`, `router`, `storage_layout` | The declaration. |
| intent | `prompt_obligation` | The prompt obligation. |

Bindings are not members: a binding's content is declared by the step that produces it, which a member already owns. The derived operation summary is analysis output and is not a member either.

## 7.3 Authoring content

The following content is classified **authoring**. It is reported in its own section, never as a semantic change, and never explains a verdict:

- an operation's `sketch` and `stage`;
- every `description` field, which the DSL defines as prose with no semantic weight;
- `intent/proposals.yaml`, the provenance of requirements;
- `project.yaml` settings other than `format` and `dsl` (the project name, the run policy).

An authoring change still appears in the diff, because a reviewer may care about it. It never contributes to a semantic change count or a gate (§15.3).

## 7.4 The partition invariant

Every leaf of a workspace's canonical serialization SHALL be owned by exactly one partition member or classified as authoring. This is enforced by a property test (§19.1): perturbing any leaf of any fixture's canonical form, wherever the result still loads, changes exactly one member's residual or exactly one authoring item.

---

# 8. Declaration changes

## 8.1 Classification

A declaration present only on head is **added**, one present only on base is **removed**, and one present on both whose residuals differ is **modified**. Residuals are compared by their canonical serialization, which one build produces deterministically.

## 8.2 Structural delta

Each modified declaration carries a **structural delta**, a readable account of what moved inside its residual. The delta is presentation: no classification, gate, or explanation reads it.

- Maps align by key.
- Lists of elements that carry an `id` align by id.
- Every other list aligns by the longest common subsequence of its elements' canonical forms, recursing into aligned elements. This keeps an inserted transaction step from reporting every later step as changed.
- Paths are printed as on the side they name: `steps[3].compare[0].field`, with base and head indexes both given where they differ.

## 8.3 Ids are identity

A declaration whose id changed is removed under its old id and added under its new one. The diff SHALL NOT pair a removal with an addition by similarity, file renames, or any other heuristic: a wrong pairing would attribute one declaration's history to another, and every later part of this programme builds on that attribution. Declared moves are Part II's to introduce.

## 8.4 Order

Changes are ordered by layer (L0, L1, intent), then by member kind in the order of §7.2, then by id.

---

# 9. Obligation identity

## 9.1 The address and the identity

An obligation keeps its `id`. The id is its address within one report, and the viz, the harness, and the engine use it as such. It stays positional and unchanged.

This revision adds a `key`: the obligation's identity across reports. The diff matches obligations by key, never by id.

## 9.2 The obligation key

```rust
pub struct ObligationKey {
    /// The report subject, without the requirement index.
    pub subject: ObligationSubject,

    pub property: Property,

    /// The value references identifying the requirement, in declared
    /// order: the key of a `SerializableBy` or `OrderedBy`, or the
    /// components of an idempotency or recoverability key.
    pub requirement_key: Vec<ValueRef>,

    /// Which of the requirements sharing subject, property, and
    /// requirement key this is, counted in declaration order.
    pub occurrence: u32,
}

pub enum ObligationSubject {
    Operation { operation: Id },
    Transaction { operation: Id, transaction: Id },
}
```

The **requirement identity** that names a `requirement` member (§7.2) is the obligation key of the obligation the requirement produces, with `property` replaced by its family.

A result replay obligation is the result half of an idempotency requirement. Its key is that requirement's key, with `property: result_replay`.

The key holds canonical value references, not a hash. It stays meaningful outside one run, and it changes only when the DSL's canonical serialization does, which happens only across a contract bump (§6.5).

## 9.3 Terms

A requirement's **terms** are the parts that do not identify it:

| Family | Identity | Terms |
|---|---|---|
| `transaction_serializability` | `key` | none |
| `transaction_ordering` | `key` | `position` |
| `idempotency` | `key` | none |
| `recoverability` | `key` | `completion` |

An obligation whose key matches across the sides but whose terms differ is **restated** (§11.3).

An idempotency requirement's `result` is not a term of the idempotency obligation, whose statement does not depend on it. Changing it from `unspecified` to `replay_consistent` adds the result replay obligation and leaves the idempotency obligation matched.

## 9.4 Why the key is the identity

The key is what the obligation is about: "executions sharing `order_id`", "attempts sharing `request_id`". Two obligations of one property on one subject with different keys are different obligations, so changing a requirement's key is a removal and an addition. Changing how strongly it is required, or against which position, is a restatement of the same one.

A key that names an input or a binding by id inherits §8.3. Renaming the input removes and re-adds every obligation keyed through it, which is honest about what the model says.

## 9.5 Relation to the design symbol key

`SymbolKey::Requirement` (`src/design/symbol.rs`) identifies a requirement by operation, family, content fingerprint, and occurrence, without naming the transaction. That is the right key for invalidation, where identical declarations are interchangeable observations. It is not reused here and is not changed by this revision.

---

# 10. The basis

## 10.1 Definition

The **basis** of a proven verdict is the set of basis facts its proof consumed. A basis fact is one of:

- a **declared fact**: one declaration's statement that the proof relied on, such as an isolation level, a lock, a guarded mutation, a delivery fact, a message identity, a commit-deduplication key, or an external boundary guarantee;
- a **query fact**: a model-wide result the proof relied on, such as the members of a conflict closure, or the absence of any transaction deleting a versioned object.

A basis is a proof's read set. It mirrors the design engine's `TaskReadSet` (`src/design/read_set.rs`), which records the symbols a task observed and the queries whose results it relied on. A proof depends on what is present and on what is absent in exactly the same way.

## 10.2 Shape of a basis fact

```rust
pub struct BasisFact {
    pub fact: Fact,

    /// The declarations the fact's truth was read from.
    pub anchors: BTreeSet<DeclarationKey>,

    /// The semantic layer of the fact: L1 for runtime declarations,
    /// L0 otherwise.
    pub layer: Layer,
}
```

`Fact` is a closed enum, one variant per kind of fact (§10.5). Two basis facts are **the same fact** when their `fact` values are equal. Anchors, and the anchors' residuals, take no part in fact equality. A fact that still holds after its anchoring declaration was modified elsewhere is the same fact. A fact has a durable form, which is what makes this equality meaningful across runs and builds (§10.9).

## 10.3 Position-free

A basis fact SHALL NOT carry a step index, a `StepLocation`, a `read_step`, or a `PathRef`. All of them are positional, and the symbol graph's own rule is that a positional location is never promoted to durable semantic identity. Where a proof structure names a step (a `LockRef`, an `AtomicConditionalMutation`), the fact names what the step does instead: its transaction, its kind, its object, its mode, its compared fields. The basis is a set, so two paths relying on one fact contribute it once.

## 10.4 Why not `assumptions`

`assumptions` remains, as the prose rendering of a proof for readers. It cannot serve as the basis:

- its wording is presentation and may change between builds;
- it embeds positions;
- it names declarations in prose, so it cannot be indexed;
- it is empty on unproven obligations, so it cannot explain a regression.

## 10.5 Catalogue

The basis is extracted from the structured proofs, never from the rendered report. At the baseline the proof structures map to facts as follows. The catalogue is normative for the baseline and grows with the proof routes. The table writes each fact in a functional shorthand named after its durable kind. The kinds and their fields are §10.9.

| Proof structure (`src/analyzer/verification/`) | Basis facts | Anchors |
|---|---|---|
| `TransactionSerializabilityProof::SerializableIsolationClosure` | `conflict_closure(root, key) = {members}` (query); `isolation(t) = serializable` for each member | the members; each member's `transaction` |
| `TransactionSerializabilityProof::ConflictGraph` | the closure query; `cyclic_components(closure)` (query); one fact per constraining `CommitOrderEvidence` below | as below |
| `CommitOrderEvidence::IntrinsicCommittedRead`, `ReadOnlyObservation` | `isolation(t) = level` | `transaction` |
| `CommitOrderEvidence::AtomicWriteOrder` | `isolation` of both sides | both `transaction`s |
| `CommitOrderEvidence::StrictLock` | `lock(t, object, selector, mode)` per side | `transaction`, `data_object` |
| `CommitOrderEvidence::AtomicConditionalMutation` | `guarded_mutation(t, object, mechanism, coverage, compared_fields)`, where `coverage` is the `GuardCoverage` kind with its read binding id and version field, and without steps | `transaction`, `data_object` |
| the observed-version leg's "no transaction deletes the object" | `no_deleters(object)` (query) | `data_object` |
| `CommitOrderEvidence::OrderedCursor` | `cursor(object, field, rule)` | `data_object` |
| `TransactionOrderingProof::Cursor`, `::Fence` | `cursor(...)` or `fence(object, field)`; and the basis of the serializability argument ordering invokes for its closure | `data_object`, the transactions advancing it |
| `ResultReplayProof::NoAdmittedInvocations`, `IdempotencyProof::NoAdmittedInvocations`, `RecoverabilityProof::NoAdmittedInvocations` | `admits_no_messages(input)` | `input`, `topic` or `outbox` |
| `IdempotencyProof::NoAdmittedPaths` | `no_admitted_paths(operation, input)` | `program`, `input` |
| `ResultReplayProof::NoReturnedResult` | `returns_no_result(operation, input)` | `program` |
| `ResultReplayProof::ClassFixedResult` | one `stable_root(operation, rule, root)` per `StabilityRule` and `ResultStabilityRule` cited (requirement key component, identified payload, recovered artifact, reconstructed artifact, replay-stable external result); `relies_on` for a replay-consistent target result (§10.6) | the requirement; the `topic` or `outbox` with its `schema`; the `transaction`; the target's `requirement`; the `effect_site` |
| `IdempotencyProof::SingleDelivery` | `delivery(input) = at_most_once` (L1); `message_identity(topic)` | `subscription_runtime`, `topic` |
| `EffectSafety::*` | per variant: `external_guarantee` for external idempotency and external side-effect freedom; `message_identity` for the same logical message, on a topic or an outbox; `relies_on` for target deduplication (§10.6); `deduplicated_by` for transaction deduplication | `effect_site`; `topic`/`outbox`; the target's `requirement`; `transaction` |
| `RetryRoute::KeyedCommit`, `Resolution::KeyedCommit` | `deduplicated_by(t, key)` | `transaction` |
| `RetryRoute::NaturalReplay`, `Resolution::NaturalReplay` | `naturally_replayable(t)` | `transaction` |
| `Resolution::TerminalStep` | `terminal(operation, input)` | `program` |
| `ArtifactReplay::Recovered`, `::Reconstructed` | `artifact_route(t, artifact, route)` | `transaction` |
| `RetryDriver::AtLeastOnceDelivery` | `delivery(input) = at_least_once` (L1) | `subscription_runtime` |
| `RetryDriver::IntrinsicOutboxRedrive` | `outbox_redrive(input)` | `input`, `outbox` |
| `RetryDriver::InboundRepeatableRequest`, `::InboundRepeatableTransitionEffect` | `retry_may_repeat(effect)` | the caller's `effect_site` or `transition` |
| `LineageFact::Propagated` | `propagation(producer, schema, source)` | the producer's `effect_site` or `transition` |

A fact whose truth was read from a schema field (its type, its optionality) also anchors that `schema`. The general rule is that a fact anchors **every declaration whose content its truth was read from**.

## 10.6 Proofs that rest on other obligations

Some proofs rest on another obligation: idempotency through a target's proven requirement (`IdempotencyProof::dependencies`), replay through a request target's replay-consistent result, and coinductive cycles. Such a proof's basis carries `relies_on(obligation key)`, anchored to that obligation's `requirement` declaration. It does not inline the other obligation's basis. A change to the other obligation is explained there, and the dependent obligation's entry names it.

## 10.7 The blocked-on set

An unproven verdict has no basis. It has a **blocked-on set**: the basis facts its obstacles found present but insufficient, the facts they found missing (stated as the fact that would be needed, such as `lock(t, object, exclusive)` missing), and their anchors. The blocked-on set is what lets a regression be explained from the head side and an improvement from the base side.

## 10.8 Exhaustiveness

Basis and blocked-on extraction SHALL match every proof, obstacle, evidence, and gap variant without a wildcard arm, so that adding a proof route without extending the catalogue fails to compile instead of producing an empty basis.

## 10.9 Durable form

Facts are stored outside the run that computed them:

- in the obligation report (§17);
- in the diff document (§14);
- in the conformance records a repository commits (Part IV), where a fact states what a piece of code was checked to provide.

A conformance record may be written by one build and read by another months later. So a fact has a durable form, held to the same standard as the spec files themselves.

### Shape

Every fact kind has a fixed list of fields (the catalogue below). A fact is a map tagged by `kind`, its fields in canonical DSL form:

- ids as strings;
- field paths as component lists;
- value references and object selectors in their canonical tagged form (§1.4 of the semantics contract);
- enums in snake case;
- sets as sorted lists.

No shorthand is written, and a reader accepts only the canonical form, so one fact has exactly one spelling. Unknown kinds and unknown fields are refused, as every other Conseqa format refuses them.

### Identity

A fact's identity is its canonical form: two facts are the same fact exactly when their canonical forms are equal. A record may also store a digest of the form, to index by. The digest is derived and never authoritative, and a reader recomputes it rather than trusting it.

This is deliberately unlike `SemanticHash`. That hash is runtime metadata, and a serialization change may legitimately move it; a fact's identity must not move.

### Versioning

`FACT_FORMAT` (in `src/analyzer/basis.rs`) counts the fact vocabulary, starting at 1. Any change to the vocabulary bumps it: a kind added, removed, or renamed, or a field added, removed, renamed, or retyped.

A `dsl` bump forces a `FACT_FORMAT` bump, since facts are stated in the DSL's vocabulary. A `FACT_FORMAT` bump never forces a `dsl` bump.

Every durable record that carries facts records both its `dsl` and its `fact_format`. A reader refuses any other pair by name, with one exception. A fact format listed in `FACT_READS` is one the current format extends without changing anything it said (a kind added, say), and it is read as the current format. This mirrors the DSL's `DSL_READS` rule. At this revision, `FACT_READS` is `[1]`.

### Anchors and the durable declaration key

Anchors (§10.2) take no part in a fact's identity. They are stored with it wherever a basis is stored, as durable declaration keys.

A declaration key has two durable forms:

- **Structured:** the `kind` / `value` map of §14.
- **Textual:** used wherever a person writes a key by hand, as Part IV's code markers do. It is the member kind of §7.2, a space, and the declaration's ids, with the ids of a contained declaration separated by `/`:

```text
transaction operation.apply_payment/tx.apply_payment
data_object data.orders/object.order
subscription_runtime operation.ship/input.ship.paid
schema schema.OrderRecord
```

Each id is written as the store writes it in a file name (`PROJECT_STATE.md`): letters, digits, `_`, `-`, and `.` (not leading) as themselves, and every other byte as `%XX`. So neither `/` nor a space ever appears inside an id, and any declaration has exactly one textual key. A `requirement` declaration has no textual form, since no one writes one by hand.

### Stability guard

Every fact kind has a golden serialization under `tests/golden/facts/`, which records the `FACT_FORMAT` it was written at. A change that alters any golden file while `FACT_FORMAT` stays the same fails the test suite. Durability does not depend on someone noticing a serde change.

### What durability does not change

Verdicts are still never compared across runs, and §6.5 stands: the diff recomputes both sides with one build and never reads an archived report as a side. The two differ in what they are. A fact is a statement in the model's vocabulary, which is stable within a fact format. A verdict is one build's judgment, which is not.

### Catalogue of fields

| Kind | Fields |
|---|---|
| `isolation` | `operation`, `transaction`, `level` |
| `lock` | `operation`, `transaction`, `data_model`, `object`, `selector`, `mode` |
| `guarded_mutation` | `operation`, `transaction`, `data_model`, `object`, `mechanism`, `coverage`, `compared_fields` |
| `cursor` | `data_model`, `object`, `field`, `rule` |
| `fence` | `data_model`, `object`, `field` |
| `conflict_closure` | `root` (an operation and a transaction), `key`, `members` (operation–transaction pairs) |
| `cyclic_components` | `root`, `key`, `components` (sets of operation–transaction pairs) |
| `no_deleters` | `data_model`, `object` |
| `admits_no_messages` | `operation`, `input` |
| `no_admitted_paths` | `operation`, `input` |
| `returns_no_result` | `operation`, `input` |
| `terminal` | `operation`, `input` |
| `stable_root` | `operation`, `rule`, `root` (a value reference) |
| `delivery` | `operation`, `input`, `semantics` |
| `message_identity` | `channel` (a topic, or a data model's outbox), `schema`, `fields` |
| `deduplicated_by` | `operation`, `transaction`, `key` |
| `naturally_replayable` | `operation`, `transaction` |
| `artifact_route` | `operation`, `transaction`, `artifact`, `route` |
| `outbox_redrive` | `operation`, `input`, `data_model`, `outbox` |
| `retry_may_repeat` | `site`: an effect site (`operation`, `effect`) or a transition effect (`machine`, `transition`, `effect`) |
| `external_guarantee` | `operation`, `effect`, `guarantee` (`idempotent`, `side_effect_free`), `identity` |
| `propagation` | `producer` (an effect site or a transition effect), `schema`, `channel` |
| `relies_on` | `obligation`, an obligation key (§9.2) |
| `realization` | `declaration` (the textual key the requirement applies to), `entry` (`id@version` of the chosen catalogue entry), `realizes` (the textual key of the declaration that entry realizes), `requirement` (the requirement's id) |

`realization` was added by Part VI (`Conseqa_Realization_Profiles_Revision_Specification.md` §12) before format 1 shipped. It states a configuration requirement of a chosen realization, not a proof fact. No basis contains one, and Part IV attests it as a required fact.

`coverage` is a tagged map, one of:

- `atomic`;
- `held_protection`;
- `observed_state { read }`;
- `observed_version { read, version_field }`;
- `locked_reader { selector, mode }`.

It names reads by their binding ids and never by step (§10.3). `selector` is the canonical object selector of the step the fact describes, so two locks one transaction takes on two instances of one object are two facts.

---

# 11. The obligation diff

## 11.1 Availability

The obligation diff requires both sides to assemble into a model and pass validation, which are the same preconditions `conseqa check` imposes before verifying. When either side does not, the obligation diff is **unavailable**. It is reported with the side, the reason (an operation without a program, or the count and first of the validation diagnostics), and the other side's obligation counts. The declaration diff is still produced, since it needs only that both sides load.

## 11.2 Status order

Statuses are ordered `proven` > `unknown` > `disproven`.

## 11.3 Classes

Every obligation of either side is in exactly one class:

| Class | Condition |
|---|---|
| `added` | Key present on head only. Carries head's status. |
| `removed` | Key present on base only. Carries base's status. |
| `regressed` | Matched; head status below base status. |
| `improved` | Matched; head status above base status. |
| `held` | Matched; proven on both; bases equal. |
| `rebased` | Matched; proven on both; bases differ. |
| `unproven` | Matched; unknown on both, or disproven on both. Carries whether the blocked-on sets differ. |

Two flags apply to any matched obligation:

- `restated`: its terms differ (§9.3);
- `scope`: its proof scope moved between `l0_only` and `runtime_dependent`. The move happens either because an L1 fact entered or left the basis, which makes the obligation `rebased`, or because the scope of an obligation it relies on moved (§10.6). In the second case the obligation can be `held`, and the entry names the relied-on obligation whose scope moved.

## 11.4 Removed obligations are never silent

Every removed obligation is listed in its own section of every output, grouped by the removed declaration that took it: a removed requirement, transaction, or operation. Removing a requirement is a legitimate design decision, and this revision does not forbid it. It makes it visible, and gateable (§15.3).

## 11.5 Rebased obligations

A rebased obligation lists its **withdrawn** facts (in base's basis, not head's) and its **introduced** facts (in head's, not base's). The motivating example of §2.2 reads:

```text
rebased  SerializableBy(order_id) on tx.apply_payment
  withdrawn   isolation(tx.apply_payment) = serializable
  introduced  isolation(tx.apply_payment) = read_committed
  introduced  guarded_mutation(tx.apply_payment, object.order,
                compare_and_set, observed_version(read.apply_payment.order, version))
```

## 11.6 Held obligations with touched anchors

A held obligation whose basis anchors include a modified declaration is listed as `held` together with the declarations touched. The proof rests on the same facts, but a reviewer of that declaration sees which proofs it carries.

---

# 12. Impact and explanation

## 12.1 Explaining declarations

For every matched obligation that is not `held` with untouched anchors, the **explaining declarations** are the changed declarations (added, removed, or modified) among:

- the anchors of base's basis or blocked-on set;
- the anchors of head's basis or blocked-on set;
- the members appearing in either side's results of the query facts those sets contain.

For an added or removed obligation, the explaining declarations are the added or removed `requirement` declaration that produced it, and the subject declarations that were added or removed with it.

## 12.2 The explanation invariant

Every `regressed`, `improved`, or `rebased` obligation, and every `unproven` obligation whose blocked-on set changed, SHALL have at least one explaining declaration.

Verdicts are a deterministic function of the model, and one build analyzes both sides (§6.5). A verdict that changed while none of the declarations it read from changed therefore means the extraction failed to anchor something the verifier read. Such an entry is listed under `unexplained`, with both sides' bases and blocked-on sets, as an analyzer defect. It is never dropped and never assigned a guessed explanation. The test corpus (§19.2) SHALL hold no unexplained entries, and an anchoring gap it finds is fixed in the catalogue (§10.5).

## 12.3 Per-declaration view

The impact section also inverts the relation. For every changed declaration it lists the obligations of either side whose basis or blocked-on set anchors it, with each one's class. This is the view a reviewer of one changed file wants: "this transaction carries these proofs, and this is what happened to each".

---

# 13. The reliance delta

## 13.1 Definition

A side's **reliance** is the union of the bases of its proven obligations, each fact counted with the number of obligations relying on it. The reliance delta compares the two sides:

- **introduced reliance**: facts relied on by some proven obligation on head and by none on base;
- **withdrawn reliance**: facts relied on by some proven obligation on base and by none on head.

Facts relied on by both sides, with different counts, are listed only in the per-obligation entries.

## 13.2 Meaning

§1.3 makes every proof conditional on the implementation conforming to the declarations it consumed. The reliance delta therefore states, model-wide, the change in what the implementation must guarantee for the head side's proofs to hold of it:

- an introduced fact is a guarantee the implementation must now provide, or must now be known to provide;
- a withdrawn fact is one no proof needs any longer. That is not a statement that the implementation may stop providing it. The fact may still be declared, may still matter to the external analysis (§10.9 of the semantics contract), and may still matter to readers. The diff says "no longer relied on", never "safe to remove".

## 13.3 Layers

Every fact carries its layer. Introduced L1 reliance is infrastructure-facing (today, delivery facts feeding `completion: guaranteed`), and Part IV binds it to infrastructure definitions.

## 13.4 Realization changes

When either side has a stack profile (Part VI §9), the diff reads `.conseqa/realization/` on both sides, through the same loader rules as the spec (§6.2). It reports:

- changes to the stack;
- per realizable declaration, changes to its derived needs (Part VI §6.3), and to its chosen realization's entry, version, options, or state (`chosen`, `automatic`, `ambiguous`, `invalid_choice`, `infeasible`);
- every declaration whose state on head is `ambiguous`, `invalid_choice` or `infeasible`, with its infeasibility report (Part VI §10.4);
- added and withdrawn `realization` facts (§10.9). This is the realization half of the reliance delta: configuration the code and infrastructure must now provide, or no longer need to.

A spec change that adds a need the chosen realization does not provide is reported here, at the spec gate, rather than discovered by an implementing agent.

---

# 14. The diff document

`conseqa diff --report` and the `diff_spec` tool emit one JSON document with its own format counter. Every struct denies unknown fields, as the obligation report does.

```json
{
  "format": 1,
  "dsl": 6,
  "fact_format": 1,
  "base": { "source": { "commit": "00c029f6…", "ref": "origin/master" }, "empty": false },
  "head": { "source": { "working_tree": true }, "empty": false },

  "declarations": {
    "changes": [
      {
        "declaration": { "kind": "transaction",
                         "value": { "operation": "operation.apply_payment",
                                    "transaction": "tx.apply_payment" } },
        "layer": "l0",
        "change": "modified",
        "delta": [
          { "path": "isolation", "base": "serializable", "head": "read_committed" },
          { "path": "steps[2]", "base": { "kind": "update" },
            "head": { "kind": "compare_and_set" } }
        ]
      }
    ],
    "authoring": [
      { "item": { "kind": "operation_sketch", "value": "operation.apply_payment" },
        "change": "modified" }
    ]
  },

  "obligations": {
    "status": "available",
    "entries": [
      {
        "key": {
          "subject": { "kind": "transaction", "operation": "operation.apply_payment",
                       "transaction": "tx.apply_payment" },
          "property": "transaction_serializability",
          "requirement_key": [ { "source": { "kind": "input",
                                             "id": "input.apply_payment.request" },
                                 "path": ["order_id"] } ],
          "occurrence": 0
        },
        "base_id": "oblig.operation.apply_payment.tx.apply_payment.transaction_serializability.0",
        "head_id": "oblig.operation.apply_payment.tx.apply_payment.transaction_serializability.0",
        "class": "rebased",
        "restated": false,
        "withdrawn": [ { "fact": { "kind": "isolation", "...": "..." }, "anchors": ["..."] } ],
        "introduced": [ { "fact": { "kind": "guarded_mutation", "...": "..." }, "anchors": ["..."] } ],
        "explained_by": [ { "kind": "transaction", "value": { "...": "..." } } ]
      }
    ],
    "unexplained": []
  },

  "impact": [ { "declaration": { "...": "..." }, "obligations": [ { "key": "...", "class": "rebased" } ] } ],

  "reliance": {
    "introduced": [ { "fact": { "...": "..." }, "layer": "l0", "relied_on_by": 2 } ],
    "withdrawn":  [ { "fact": { "...": "..." }, "layer": "l0" } ]
  },

  "realization": {
    "stack": { "added": ["kafka@3.7"], "removed": [] },
    "declarations": [
      {
        "declaration": "topic_runtime topic.payment_captured",
        "needs": { "added": [{ "ordering": "within_group" }], "removed": [] },
        "base": null,
        "head": { "state": "automatic", "entry": "kafka.keyed-topic@1", "options": [] }
      }
    ],
    "unrealizable": [],
    "requirements": {
      "introduced": [ { "fact": { "kind": "realization", "...": "..." } } ],
      "withdrawn": []
    }
  }
}
```

When the obligation diff is unavailable, `obligations` is `{ "status": "unavailable", "side": "base", "reason": "…", "other_side": { "proven": 11, "unknown": 2, "disproven": 0 } }`, and `impact` and `reliance` are absent.

`realization` (§13.4) is present when either side has a stack profile, and absent otherwise. It does not depend on the obligation diff being available, since needs and choices are derived from the spec and the realization files alone.

`base_id` and `head_id` link each entry to the side's obligation report, which uses the positional id as its address.

---

# 15. `conseqa diff`

## 15.1 Synopsis

```text
conseqa diff [<base>] [<head>] [--merge-base] [--report <path>] [--fail-on <class>[,<class>…]]
```

- `<base>` defaults to `HEAD`. `<head>` defaults to the working tree. So `conseqa diff` with no arguments shows what the uncommitted edits change, the way `git diff` does.
- `--merge-base` replaces `<base>` with the merge base of `<base>` and `<head>` (the working tree counts as `HEAD`). This is the form a pull request wants: `conseqa diff origin/master HEAD --merge-base` reports what the branch changed, not what the target branch changed since the branch point.
- `--report <path>` writes the diff document (§14), mirroring `conseqa check --report`.
- Like every `conseqa` subcommand after initialization, `diff` takes no project path: the project is discovered from the working directory (§6.3).

## 15.2 Human output

```text
base  origin/master (00c029f)    head  working tree

declarations   3 modified, 1 added    l0 3, l1 1    authoring 1
  ~ transaction  operation.apply_payment / tx.apply_payment
      isolation  serializable -> read_committed
      steps[2]   update -> compare_and_set (compare: version)
  ~ subscription_runtime  operation.ship / input.ship.paid
      delivery  at_least_once -> unspecified
  …

obligations    14 matched: 10 held, 2 rebased, 1 regressed, 1 unproven; 1 added; 0 removed
  ! regressed  Recoverability(event_id), completion: guaranteed on operation.ship
      proven -> unknown
      explained by  ~ subscription_runtime operation.ship / input.ship.paid
  ~ rebased    SerializableBy(order_id) on tx.apply_payment
      withdrawn   isolation(tx.apply_payment) = serializable
      introduced  guarded_mutation(tx.apply_payment, object.order, compare_and_set, observed_version)
  + added      Idempotency(refund_id) on operation.refund    unknown
  …

reliance
  + guarded_mutation(tx.apply_payment, object.order, compare_and_set, observed_version)   2 obligations
  - isolation(tx.apply_payment) = serializable                                             no longer relied on
  - delivery(input.ship.paid) = at_least_once   (l1)                                       no longer relied on
```

Removed obligations and unexplained entries, when there are any, are printed in their own sections after the obligations.

## 15.3 Gates and exit codes

`--fail-on` names the classes that fail the run:

| Gate | Fails when |
|---|---|
| `regressed` | any obligation regressed |
| `removed` | any obligation was removed, whatever its base status |
| `added-unproven` | any added obligation is not proven |
| `rebased` | any obligation was rebased |
| `unexplained` | any entry is unexplained |
| `infeasible` | any declaration on head is `infeasible`, or has an `invalid_choice` (§13.4) |
| `ambiguous` | any declaration on head is `ambiguous` (§13.4) |

| Exit | Meaning |
|---|---|
| 0 | The diff was produced, and no named gate failed. |
| 1 | The diff could not be produced: a side did not load, was refused (§6.2, §6.5), or named an unresolvable ref, or a git side was named outside a git repository. |
| 2 | A named gate failed, or a gate was named while the obligation diff was unavailable. A gate that cannot be evaluated fails. |

Exit 2 matches `conseqa check --require-proven` and `conseqa design`: the command ran and the result is not acceptable. `conseqa diff` does not replace `conseqa check`. CI runs both: `check` judges the head side alone, and `diff` judges the change.

---

# 16. Design MCP tool `diff_spec`

The design MCP server gains `diff_spec`:

| Parameter | Meaning |
|---|---|
| `base` | A git commit expression. Default `HEAD`. |
| `merge_base` | As `--merge-base`. Default `false`. |

The head side is the project's current files, which the engine adopts as its head before every tool call (`PROJECT_STATE.md`). The result is the diff document with a short text summary in front. The tool is read-only, takes no part in the commit gate, and is a shared tool, served to every role as `requirement_report` is. A design agent finishing a task can ask what its work changed relative to the branch it started from, and see a regression before a reviewer does.

---

# 17. Obligation report format 10

`ProverReport.format` becomes 10. `ProverReport` gains `fact_format`, the `FACT_FORMAT` its facts are written in (§10.9), beside its existing `dsl`. `Obligation` gains:

| Field | Present | Meaning |
|---|---|---|
| `key` | always | The obligation key (§9.2). |
| `basis` | proven only | The basis (§10). |
| `blocked_on` | unproven only | The blocked-on set (§10.7). |

`id`, `assumptions`, and `evidence` are unchanged. The `FORMAT` doc comment gains its format 10 line. The viz's report types (`viz/src/types/report.ts`) accept the new fields; nothing in the viz is required to read them in this revision. `conseqa check --report` thereby carries the basis, so a single report already answers "which proofs rest on this declaration" without a diff.

---

# 18. Implementation changes by module

## 18.1 `src/design/store.rs`

Factor `load_workspace` over a file source with two implementations: the directory (today's behavior) and a git tree read through `git rev-parse`, `git ls-tree -r -z`, and `git cat-file --batch`. Every refusal goes through the same code for both. Add the empty-project reading of §6.4.

## 18.2 `src/analyzer/basis.rs` (new)

Defines `ObligationKey`, `BasisFact`, `Fact`, the query facts, and the exhaustive extraction from `VerificationReport` (§10.5, §10.8). Also defines the durable form (§10.9): `FACT_FORMAT`, `FACT_READS`, the canonical serialization and its two-phase probe of `dsl` and `fact_format`, and the textual declaration key with its parser. It lives in the core library, not behind the `design` feature, because the report needs it.

## 18.3 `src/analyzer/report.rs`

Compute `key` for every obligation in `scaffold`, and fill `basis` and `blocked_on` in `obligations`. Bump `FORMAT` to 10.

## 18.4 `src/analyzer/verification/`

Wherever an argument rests on an absence that the structured proof does not yet state (the observed-version leg's "no transaction deletes the object" is the known case), expose it in the proof or check structure so that extraction can turn it into a query fact. No verdict changes.

## 18.5 `src/design/change/` (new, `design` feature)

- `partition.rs`: the declaration partition, residual projection, and authoring classification (§7).
- `delta.rs`: structural deltas (§8.2).
- `diff.rs`: declaration changes, the obligation diff, explanation, impact, reliance, and the document (§8, §11–§14).

## 18.6 `src/main.rs`

The `diff` subcommand (§15).

## 18.7 `src/design/mcp.rs`

The `diff_spec` tool (§16).

## 18.8 `viz/src/types/report.ts`

Optional `key`, `basis`, and `blocked_on` fields.

---

# 19. Test plan

## 19.1 Partition invariant

For every fixture under `tests/fixtures/`, perturb each leaf of the workspace's canonical form. Wherever the result loads, exactly one member's residual or exactly one authoring item changes (§7.4).

## 19.2 Explanation corpus

For every fixture, apply each mutation of a fixed catalogue: remove a lock step; drop or weaken isolation; replace a `compare_and_set` with an `update`; add a transaction deleting a versioned object; change a subscription's delivery; remove a `deduplicated_by`; change an external effect's idempotency; widen a selector; remove a requirement. Diff the fixture against the mutation. Assert that the expected obligations change class and that no entry is unexplained (§12.2).

## 19.3 Reordering is not a change

Reverse the order of an operation's requirements and of a transaction's requirements. The declaration diff and the obligation diff are both empty, though the positional ids differ.

## 19.4 Rebasing

Replace `isolation: serializable` with a read-committed `compare_and_set` of the observed version, as in §2.2. The obligation is `rebased`, with the withdrawn and introduced facts of §11.5. The reliance delta introduces the guarded mutation and withdraws the isolation fact.

## 19.5 Runtime scope

Change a subscription's delivery from `at_least_once` to unspecified. The guaranteed recoverability obligation citing it is `regressed`, explained by the `subscription_runtime` declaration. The delivery fact is withdrawn reliance, at layer L1.

## 19.6 Authoring only

Change only a sketch, a stage, and a description. The authoring section lists them; the declaration changes and the obligation diff are empty.

## 19.7 Absence

Add a transaction deleting `object.order` to a model whose serializability proof uses the observed-version leg. The affected obligations are explained by the added transaction through the `no_deleters(object.order)` query fact.

## 19.8 Ids are identity

Rename a transaction. Its declaration and its obligations are removed and re-added, and nothing is paired.

## 19.9 Restatement and result replay

Change a recoverability requirement's completion from `resumable` to `guaranteed`: matched, `restated`. Change an idempotency requirement's `result` from `unspecified` to `replay_consistent`: the idempotency obligation stays matched, and the result replay obligation is `added`.

## 19.10 Sources and refusals

Cover each of the following:

- the working tree against `HEAD`;
- two commits;
- `--merge-base`;
- a base without a project, read as empty;
- a side at another `dsl` or `format`, refused by name with exit 1;
- an unresolvable ref, exit 1;
- a broken file on the working tree side, exit 1, naming the file;
- no file written, no lock taken, and no index change in any of these.

## 19.11 Gates

Each `--fail-on` class fails exactly when §15.3 says. A gate named while the obligation diff is unavailable exits 2.

## 19.12 Determinism

The same two trees produce byte-identical documents across two runs.

## 19.13 Report format 10

A proven obligation carries a non-empty basis, and an unproven one a non-empty blocked-on set. Every key round-trips. The viz loads a format 10 report.

## 19.14 Durable facts

- Every fact kind has a golden serialization, and the test fails when one changes without a `FACT_FORMAT` bump (§10.9).
- Every fact extracted from every fixture round-trips through its canonical form unchanged.
- A shorthand spelling, an unknown kind, an unknown field, and a record of another `dsl` or `fact_format` are each refused by name.
- Every textual declaration key round-trips through its parser, including ids that need `%XX` escapes.

## 19.15 Realization changes

With a stack profile on both sides, each of the following is reported in the realization section, and fails the named gate:

- adding a lock step to a transaction on a DynamoDB-realized data model makes it `infeasible` (`infeasible`);
- adding SQS to a Kafka-only stack makes every topic runtime `ambiguous` (`ambiguous`);
- choosing `kafka.keyed-topic@1` introduces its `record-key` facts on every producing effect site, as introduced realization requirements.

A side without a stack profile produces no realization section, and neither gate fails.

---

# 20. Acceptance criteria

1. `conseqa diff` produces the document of §14 for any two loadable sides of the building binary's format and DSL, and refuses every other side by name.
2. Reading a git side writes nothing and takes no lock.
3. Every semantic change is attributed to exactly one declaration. Authoring changes are reported apart and never count as semantic.
4. Obligations are matched by key. Reordering requirements changes no class.
5. Proven-on-both obligations are split into `held` and `rebased` by basis equality.
6. Removed obligations are always listed, and gateable.
7. No entry is unexplained across the explanation corpus.
8. The reliance delta lists introduced and withdrawn facts with their layers.
9. Obligation report format 10 carries `key`, `basis`, and `blocked_on`. Positional ids are unchanged.
10. `diff_spec` serves the same document to every design MCP role.
11. No verdict of any fixture changes.
12. Facts have one canonical durable form per kind, versioned by `FACT_FORMAT` and guarded by golden tests. Declaration keys have one textual form.
13. With a stack profile, the diff reports realization changes and unrealizable declarations, and the `infeasible` and `ambiguous` gates fail on them (§13.4).

---

# 21. Later parts (outline, not normative)

## Part II: Deployment baseline, declared moves, and durable-state impact

- **Baseline.** A deployed environment is named by a git ref (a convention such as `conseqa/deployed/<env>` tags), so "the implications for the deployed application" becomes `conseqa diff conseqa/deployed/prod`.
- **Declared moves.** A `moved_from` declaration lets a rename keep its history. §8.3 refuses to guess one, and every later part needs the attribution.
- **Change classes.** Each declaration change is classified for deployment: additive; needs backfill; needs drain; breaking for external callers; unsafe.
- **Durable-state impact.** For each change, the inventory of durable state it meets:
  - rows of changed data objects;
  - undrained outbox messages;
  - retained topic messages;
  - established effect intents and transaction outputs;
  - `deduplicated_by` and request-identity records;
  - state machine instances in a removed or renamed state;
  - cursor and fence positions.

  Conseqa models all of these, so it can list them for a change, which a schema-diff tool cannot.

## Part III: Logical migrations

- **The migration artifact.** A data-model change produces a checked logical migration, not SQL. It is a total mapping from base's objects, fields, and states to head's, with a value for every new required field, a state for every removed one, and declared splits and merges.
- **Derived step order.** The partition's reader and writer sets order the steps against code deploys. Every transaction that inserts or upserts an object must write a new required field before it is enforced, and every reader of the field ships after its backfill. That is expand / migrate / contract, derived rather than remembered.
- **Data preconditions.** A changed identity is safe only if the new identity is unique over existing rows, which no static check can know. The migration emits it as an assertion to run against the environment first.
- **Lowering.** Translation to a concrete store is an adapter, outside the semantics.

## Part IV: Realization bindings

Drafted in full, ahead of Parts II and III, as `Conseqa_Conformance_and_Implementation_Guidance_Revision_Specification.md`, which supersedes this outline. In brief:

- **Markers.** Comment markers in any text file (code, SQL, configuration, Terraform) bind regions to declarations.
- **Records.** Per-declaration conformance records store the residual the code was checked against, the region digests, and the attested facts with their evidence. Re-checked completeness claims cover the model-wide facts.
- **Guidance.** A derived work list and work items guide implementing agents. Each item names the load-bearing facts, what each one's withdrawal would cost, and the project's own precedents.
- **Conformance.** Every proven obligation is reported as conformant in the code or not.
- **Part II.** The deployment baseline becomes the records at the deployed commit.

## Part V: Mixed-version verification

- **Why.** During a rollout, base and head operations run against shared data and channels, so idempotency, recoverability, and result replay become properties that span versions. Spec A valid and spec B valid does not make A → B safe:
  - Adding a `version` to an object defeats B's version guards while A's writers, which do not publish, still run.
  - Changing an external effect's idempotency key derivation lets B re-execute A's established intents under new keys.
  - Reshaping a `deduplicated_by` key makes it stop matching A's records.
- **The transition model.** Both versions' operations over the expanded data model, verified by the existing verifiers, with rollback as the same check reversed. A change whose transition model fails is split into intermediate specs, each of which passes.
- **Semantics.** This is new proof territory, and needs a semantics revision and a §27 entry.

## Part VI: Realization profiles

Specified in `Conseqa_Realization_Profiles_Revision_Specification.md`. It covers:

- a capability vocabulary derived from the spec;
- a catalogue of technologies stating what each provides and requires;
- a per-project stack profile;
- a chosen realization per declaration, with feasibility checked at the spec gate (§13.4);
- realization requirements attested under Part IV.

The DSL is unchanged.

---

# 22. Documentation reconciliation

When this part is implemented:

- `docs/live/` gains `SPEC_CHANGE_ANALYSIS.md`, describing the partition, obligation keys, the basis, the diff, and its surfaces against the implementation, and the live corpus table in `docs/live/README.md` lists it;
- `PROJECT_STATE.md` adds `conseqa diff` to its table of surfaces;
- §25.1 of `CONSEQA_DSL_SEMANTICS.md` gains a paragraph defining the basis as the structured form of "the declarations it consumed", and §2's `Revision` paragraph notes that change analysis compares trees, never revisions;
- this document then becomes a historical design record, listed in the README's historical table.
