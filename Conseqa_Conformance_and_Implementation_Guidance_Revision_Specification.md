# Conseqa Conformance and Implementation Guidance Revision Specification

## 1. Status

**Status:** Proposed  
**Part:** IV of the spec-versioning programme: realization bindings. It is drafted ahead of Parts II and III, because Part II's deployment baseline is defined in terms of the records this part introduces (§16).  
**Depends on:** Part I, `Conseqa_Spec_Change_Analysis_Revision_Specification.md`. It uses Part I's declaration partition (§7), obligation keys (§9), the basis and the durable form of facts (§10, §10.9), and the reliance delta (§13).  
**Amended by:** Part VI, `Conseqa_Realization_Profiles_Revision_Specification.md` (§7.1–§7.3, §8.2, §10.1, §12.1, §12.2, §14.3, §14.4, §19, §21). Realization requirements become required facts, a stack profile brings realized declarations under binding, and catalogue checkers record reviews.  
**Target DSL version:** 6 (unchanged)  
**Project store format:** 10 (unchanged). Conformance records live in `.conseqa/` but outside what the store loads and scans (§8.1).  
**Conformance format:** 1 (new)  
**Baseline:** Conseqa `master`, commit `8883890` (PR #38 merged)  
**Primary affected subsystems:** a new conformance module, the `conseqa` CLI, the design MCP server, the harness backends  
**Amended in implementation (Part I's M3, 2026-10-30):** §12.2's `guarded_mutation` withdrawal applies to a `compare_and_set` only, with the maintainer's approval, as Part I's mutation catalogue implements it.  
**Amended in implementation (M4, 2026-10-30):** with the maintainer's approval:
- §6.1–§6.3 and §6.5: the marker grammar and pairing, region residuals, and the scan's root and what it skips;
- §8.1, §8.2, §8.4 and §8.6: refusals fail the read, the record's format rules, the write lock and digest, and missing version fields;
- §14.3, §14.4, §15.3 and §17.1: review lapse, the policy's defaults, exit 1, and the module table.

**Amended in implementation (M5, 2026-10-31):** with the maintainer's approval:
- §7.1–§7.3: `keyed_requirement`'s class, structural facts required of their bindable anchors, and the implementers of a message identity, a cursor, and a fence;
- §8.2 and §9.2–§9.4: patterns compile as regular expressions, how a match is placed, exceptions, the objects a closure accesses, and when a required scope holds;
- §10.1 and §10.2: every condition's exact rule, records naming no declaration, conditions without a record, and when each class of fact holds of the code;
- §14.3, §14.4, §15.3 and §17.2: review verdicts, what needs a review, an unverifiable spec, and the basis's implementer rule.

This revision connects a spec to the code that implements it, and turns the spec's changes into guided implementation work for LLM agents. It introduces:

- **binding markers**: comment pairs, in any text file of the repository, that bind a region of the file to a declaration;
- **conformance records**: one committed file per declaration, stating what the declaration's regions were checked to provide, with evidence and provenance;
- **completeness claims**: re-checked statements that no unbound code touches what a model-wide fact depends on;
- the **conformance state** of every declaration and every obligation, derived from the spec, the code, and the records;
- the **work list** and **work items**: ordered, self-contained implementation tasks. Each one states what to change, where, which details are load-bearing and why, and how this project already implements them;
- **reviews**: independent checks of a record by a fresh-context agent;
- `conseqa conform` subcommands and design MCP tools serving all of it.

Conseqa still writes no application code. It tells an agent what the code must do and records what was checked.

---

# 2. Motivation

## 2.1 A proof is only as good as the code's conformance

Every Conseqa proof is conditional on the implementation conforming to the declarations it consumed (§1.3, §25 of the semantics contract). Today nothing connects a declaration to any code. Conformance is an unrecorded hope: a proven `SerializableBy(order_id)` says nothing about whether the repository function behind `tx.apply_payment` performs the conditional update the proof rests on.

## 2.2 LLMs delete load-bearing detail

Agents implement and refactor most code now, and their typical failure against a spec is not building the wrong feature. It is removing a detail that looks incidental and is in fact load-bearing:

- a `version = $2` in a `WHERE` clause that looks redundant;
- a unique index that looks like an optimization;
- a `FOR UPDATE` that looks like a performance mistake;
- an idempotency key on a payment call that looks optional;
- a queue's redrive policy that looks like noise in the Terraform.

Each one silently falsifies a proven property. An agent cannot know which details are load-bearing unless something tells it, at the place it is editing, along with the reason.

## 2.3 Code lags the spec piece by piece

A spec change may touch ten declarations. The code catches up over several pull requests, while further spec changes land. "The spec version this code implements" does not exist as one number. Each part of the code was last brought into line with a different version of the declaration it implements. A diff between two spec versions (Part I) says what changed in the spec, not how far the code has caught up.

## 2.4 What Part I leaves open

Part I's reliance delta lists the facts the implementation must newly guarantee. But "the implementation" there has no address. Nothing says where a fact is implemented, whether anyone checked, or what the code looked like when they did.

---

# 3. Goals

## 3.1 Conformance is recorded, in the repository

What was checked, against which content, by whom, is committed beside the spec and reviewed in the same pull requests.

## 3.2 The baseline is per declaration

There is no global "implemented version". Each record names the declaration content its code was checked against, so each piece of code carries its own baseline.

## 3.3 Work is derived, never stored

The state and the work list are a pure function of the spec files, the repository's files, the records, and the policy. No task log, no machine-local state. A branch has its own work list, and a merge's work list is derived from the merged files.

## 3.4 Load-bearing detail is visible where it is edited

A work item names every fact a proof needs from the code it covers, the obligations relying on each one, and what becomes unproven without it.

## 3.5 The project teaches its own idioms

How this repository already implements a `compare_and_set`, a strict lock, or a commit-deduplication key is evidence in its existing records. A work item cites those precedents.

## 3.6 The tool computes what it records

Region digests and the declaration snapshot are computed by the tool from the files, never supplied by the agent. An agent can describe evidence, but it cannot claim conformance for code the tool did not see.

## 3.7 Any text file

Code, SQL, configuration, and Terraform are bound the same way. No language parser is needed.

## 3.8 One answer for agents and CI

The design MCP server and the `conseqa conform` CLI serve the same derivation.

---

# 4. Non-goals

This revision does not:

- generate code;
- analyze code semantically. Whether a region provides a fact is judged by review, by an agent or a person, not proved. Deterministic checkers for fact kinds with a recognizable code shape are deferred (§21);
- monitor anything at runtime or inspect deployed infrastructure. Infrastructure definitions are bound like any other file; deployed resources are not queried;
- parse any language. Regions are delimited by explicit markers (§20);
- authenticate who attested. Provenance is a claim, reviewed in the pull request that commits it (§14.5);
- change the DSL, any verdict, the store format, or Part I's diff;
- model deployments, migrations, or mixed-version execution (Parts II, III, V).

---

# 5. Terms

| Term | Meaning |
|---|---|
| **Binding marker** | A `conseqa:begin` / `conseqa:end` comment pair naming a declaration (§6). |
| **Region** | The lines between one begin marker and its end marker, in one file. |
| **Bound declaration** | A declaration with at least one region. |
| **Conformance record** | The committed file stating what a declaration's regions were checked to provide (§8). |
| **Implementer** | A declaration whose code must provide a given fact (§7.2). |
| **Required fact** | A fact the current spec's proofs rely on, of an attestable kind, with a given declaration among its implementers (§7.3). |
| **Structural conformance** | The claim that a declaration's regions implement its residual and do no modeled work beyond it (§7.1). |
| **Completeness claim** | A re-checked claim that every occurrence of given patterns lies in regions of declared writers or publishers (§9). |
| **Condition** | One derived fact about a declaration's conformance, such as `stale` or `missing_facts` (§10). |
| **Work item** | The unit of implementation work for one declaration, derived from its conditions (§11, §12). |
| **Precedent** | Evidence for the same fact kind in another current, reviewed record (§12.3). |
| **Review** | An independent check of a record at specific region digests (§14.3). |

---

# 6. Binding markers

## 6.1 Syntax

```text
<anything> conseqa:begin <declaration key> [<closer>]
…
<anything> conseqa:end <declaration key> [<closer>]
```

A marker is recognized wherever `conseqa:begin` or `conseqa:end` appears on a line, so prose or a string literal that mentions a keyword is a malformed marker. Whatever precedes the keyword is the comment leader, and is ignored: `//`, `#`, `--`, `;`, `/*`, `<!--`. Then come:

1. whitespace;
2. the key;
3. optionally, whitespace and one comment closer (`*/`, `-->`). The closer may touch the key, as in `…/tx.reserve*/`.

Anything else after the key is malformed.

The key is Part I's textual declaration key (§10.9): the member kind, exactly one space, and the escaped ids joined by `/`. The end marker repeats the key, so a mismatched pair is caught instead of silently closing the wrong region.

```rust
// conseqa:begin transaction operation.apply_payment/tx.apply_payment
pub async fn apply_payment(db: &mut PgTransaction<'_>, cmd: &ApplyPayment) -> Result<Applied> {
    let order = sqlx::query_as!(Order, "SELECT * FROM orders WHERE order_id = $1", cmd.order_id)
        .fetch_one(&mut **db).await?;
    let updated = sqlx::query!(
        "UPDATE orders SET status = 'paid', version = version + 1
         WHERE order_id = $1 AND version = $2",
        cmd.order_id, order.version,
    ).execute(&mut **db).await?;
    …
}
// conseqa:end transaction operation.apply_payment/tx.apply_payment
```

```hcl
# conseqa:begin subscription_runtime operation.ship/input.ship.paid
resource "aws_sqs_queue" "ship_paid" {
  redrive_policy = jsonencode({ deadLetterTargetArn = aws_sqs_queue.ship_paid_dlq.arn, maxReceiveCount = 10 })
}
# conseqa:end subscription_runtime operation.ship/input.ship.paid
```

## 6.2 Regions

A region is the lines strictly between a begin marker and its matching end marker in one file:

- Regions nest: a `program` region may contain the `transaction` regions of its transactions.
- Two regions of one file may not overlap without one containing the other.
- A declaration may have any number of regions, in any number of files. A transaction is often a repository function, a SQL file, and a migration.
- Each region binds exactly one declaration.

Markers pair as brackets do:

- An end marker closes the innermost open region when its key is that region's.
- If its key names an outer open region instead, the two regions overlap.
- If its key names no open region, it is an end whose key differs from its begin's, and it closes the innermost region.

A marker whose key does not parse still pairs by its text. A bad key is therefore reported at each marker carrying it, and raises no pairing error. A region may contain another region of its own declaration.

## 6.3 Region digest

A region's digest is the BLAKE3 hash of its **region residual**:

- the region's text, with line endings normalized to `\n`. Only `\n` and `\r\n` end a line;
- each directly nested region, markers included, replaced by one line holding exactly its textual key, with no comment leader or indentation.

The digest therefore mirrors Part I's partition. Editing a transaction's region drifts the transaction and not the program around it, and editing the program around the transaction drifts only the program. Re-indenting or re-commenting the transaction's markers drifts neither.

## 6.4 Bindable declarations

Every member of Part I's declaration partition (§7.2) is bindable, except two:

- `requirement`, since requirements are discharged by proofs, not by code;
- `prompt_obligation`, which is intent.

## 6.5 Scan scope

The scan reads:

- the files git tracks (`git ls-files`);
- the untracked files git does not ignore (`git ls-files --others --exclude-standard`).

`.conseqa/` itself is never scanned, and neither is a file that is not valid UTF-8. Outside a git repository, the scan walks the project root, skipping `.conseqa/` and dot-directories. A scan reads only, and writes nothing.

The scan covers the project directory, the one holding `.conseqa/`, since that is where the code a project describes lives. In git, it lists the files below that directory. It names each file relative to the project directory, with `/` separators. It never reads:

- a symbolic link, which would read another file's lines under a second name;
- a submodule, which is another repository;
- a tracked file missing from the working tree;
- a path that is not UTF-8, which no record could name.

## 6.6 Diagnostics

The scan reports, by file and line:

- a malformed marker;
- a key that does not parse;
- a begin without an end, or an end without a begin;
- an end whose key differs from its begin's;
- overlapping regions;
- a key naming a kind that is not bindable.

These are **scan errors**. A file with one contributes no regions. No record whose regions or evidence lie in that file can be written, and `conseqa conform check` fails while any scan error remains. A marker whose key parses but names no declaration in the current spec is not a scan error. It is an `orphaned` condition (§10.1): the code still implements something the spec no longer declares.

---

# 7. Facts and their implementers

## 7.1 Conformance classes

Every fact kind of Part I §10.9 belongs to exactly one conformance class:

| Class | How the code provides it | Kinds |
|---|---|---|
| **attestable** | A region of an implementer provides it, and a record attests it with evidence. | `isolation`, `lock`, `guarded_mutation`, `cursor`, `fence`, `deduplicated_by`, `delivery`, `message_identity`, `outbox_redrive`, `retry_may_repeat`, `external_guarantee`, `propagation`, `realization` |
| **structural** | It follows from its anchors' content, so it holds of the code when each anchor that can be bound conforms structurally. | `naturally_replayable`, `artifact_route`, `admits_no_messages`, `no_admitted_paths`, `returns_no_result`, `terminal`, `stable_root` |
| **model-wide** | It depends on code the model does not declare doing nothing to the objects involved, so it holds only under completeness claims (§9). | `conflict_closure`, `cyclic_components`, `no_deleters` |
| **internal** | A statement about the spec, which no code provides. A relied-on obligation holds when it is conformant itself (§10.2). A requirement declared is only ever stated missing, in a blocked-on set, and no basis holds one. | `relies_on`, `keyed_requirement` |

**Structural conformance** is the claim that a declaration's regions implement its residual and perform no modeled work beyond it: no read, mutation, lock, effect, publication, or outbox write the residual does not declare. The "beyond it" half is what makes a structural fact hold of the code. A transaction is naturally replayable because of what it does not do, so an extra `INSERT` in its code falsifies the fact, though every declared step is present.

A structural fact's anchors are the declarations whose content its truth was read from (Part I §10.5), so it holds of the code when each of them that can be bound conforms structurally. A `requirement` anchor has no code: a root stable because it is a key component asks nothing of any region.

## 7.2 Implementers

| Kind | Implementers |
|---|---|
| `isolation`, `lock`, `guarded_mutation`, `deduplicated_by` | the `transaction` |
| `cursor`, `fence` | every `transaction` with a step on the field: an `advance_cursor` step for a cursor, a `fence` step for a fence |
| `delivery` | the `subscription_runtime` |
| `message_identity` | every `effect_site` publishing or writing that schema to the channel, and every `transition` whose side effect publishes it there |
| `outbox_redrive` | the `outbox`: whatever relays it must durably re-drive |
| `retry_may_repeat` | the caller's `effect_site`, or the `transition` |
| `external_guarantee` | the `effect_site` |
| `propagation` | the producer's `effect_site`, or the `transition` |
| `realization` | the declaration named in its `declaration` field (Part VI §11.3) |

A fact with several implementers is required of each of them, and each one's record attests it for its own regions. A fact with none in the model, such as the identity of a message no modeled site publishes, asks nothing of implementers, though the claims it requires still apply (§9.4).

## 7.3 Required facts

A declaration's **required facts** come from two sources:

- the attestable facts in the current spec's reliance (Part I §13.1) whose implementers include it. These are what the current proofs need its code to provide;
- when the project has a stack profile, every `realization` fact applying to it (Part VI §11). These are the configuration its chosen realizations demand of it, whether or not a proof relies on them.

A record attests only required facts (§14.2). The structural claim covers the rest of the declaration's content.

A structural fact in the current reliance is required of each of its anchors that can be bound (§7.1). Such a declaration provides it with its structural claim, `structural: true`, rather than by attesting it. Without the claim, it has the condition `missing_facts` (§10.1).

A declaration that a catalogue entry realizes has its chosen entry recorded in its record (§8.2). When the chosen entry, its version, or its options change, the record is `stale` (§10.1): the claims it was checked against are no longer the ones in force.

## 7.4 Evidence

Each attested fact cites at least one evidence span: a file and a line range. A span must lie inside a region bound to the implementer, or to one of the fact's anchors. That lets a database-wide default isolation, bound to the `data_model`, evidence a transaction's isolation. Spans are presentation: their line numbers move as files change, and a record's validity rests on the region digests (§6.3), not on the spans.

---

# 8. Conformance records

## 8.1 Location

```text
.conseqa/
  conformance/
    policy.yaml                                   optional (§14.4)
    <kind>/<id>[/<id>].yaml                       one record per declaration
```

The path is the declaration's textual key: the kind as a directory, then each escaped id as a path segment, the last with `.yaml` appended. For example, `conformance/transaction/operation.apply_payment/tx.apply_payment.yaml`.

The conformance reader follows the store's strictness rules. A directory that is not a bindable kind is refused by name, and so are a file name that is not an id's canonical encoding and any stray entry. Dotfiles pass.

A refusal fails the read and names the entry, as a spec file that does not load fails the store's. So does a record that does not parse or breaks a rule of its format (§8.2). Only a record of another version is set aside instead (§8.6).

The store loads `project.yaml`, `spec/`, and `intent/`, and its change detection scans only those (`src/design/store.rs`). Records therefore sit outside the spec in three ways. Writing one never adopts a spec revision, never invalidates a design task, and never needs the spec writer lock. Older builds ignore the directory.

## 8.2 Content

```yaml
format: 1                 # conformance format
dsl: 6
fact_format: 1
declaration: transaction operation.apply_payment/tx.apply_payment

residual:                 # the declaration's residual (Part I §7.2) when checked, canonical
  data_model: data.orders
  isolation: read_committed
  steps: [ … ]

regions:                  # computed by the tool (§14.2)
  - path: src/payments/apply.rs
    lines: [42, 71]       # its begin and end markers' lines: presentation
    digest: 9f2c…         # §6.3
  - path: migrations/0007_orders_version.sql
    lines: [1, 12]
    digest: 41d0…

structural: true          # §7.1

# realization: { entry: kafka.keyed-topic@1, options: [] }
#   present only on records of declarations a catalogue entry realizes (Part VI §10)

facts:
  - fact:
      kind: guarded_mutation
      operation: operation.apply_payment
      transaction: tx.apply_payment
      data_model: data.orders
      object: object.order
      mechanism: compare_and_set
      coverage: { kind: observed_version, read: read.apply_payment.order, version_field: [version] }
      compared_fields: [[version]]
    evidence:
      - { path: src/payments/apply.rs, lines: [47, 51] }
  - fact:                 # a realization requirement of the data model's PostgreSQL entry (Part VI §11)
      kind: realization
      declaration: transaction operation.apply_payment/tx.apply_payment
      entry: postgres.database@1
      realizes: data_model data.orders
      requirement: conditional-update
    evidence:
      - { path: src/payments/apply.rs, lines: [47, 53] }

completeness: []          # data_object, topic, and outbox records only (§9)

attested:
  by: { kind: agent, name: claude-opus-5-5 }
  commit: 8883890…        # informational: the commit the attester's tree was based on

reviews:
  - by: { kind: agent, name: codex }
    digests: [9f2c…, 41d0…]
    verdict: confirmed    # confirmed | disputed
    items:                # optional per-fact or per-claim verdicts
      - { fact: 0, verdict: confirmed, note: "WHERE version = $2 at 50" }
      - { structural: true, verdict: confirmed }
```

The format's rules beyond its shape:

- `by.kind` is `agent`, `person`, or `checker`. `attested.commit` is optional, and absent outside git.
- Each attested fact is in its canonical durable form, appears once, and cites at least one evidence span.
- A `writers` claim belongs on a `data_object` record, and a `publishers` claim on a `topic` or `outbox` record. Each claim names at least one pattern, and each pattern compiles as a regular expression (§9.2).
- A review item names exactly one target: `fact: <index>`, `completeness: <index>`, or `structural: true`. Each index lies within the record.
- Every path is relative to the project directory, as the scan names files.

## 8.3 Why a snapshot, not a hash

A record stores the declaration's residual itself, not its digest. "What changed in this declaration since its code was checked" must be answerable whatever the repository's history looks like after squash merges, rebases, or a shallow clone. A digest would answer only whether something changed. A residual is bounded by one declaration, so the size is modest.

## 8.4 Writing

Records are written by the attest and review operations (§14). Each write is atomic: a temporary sibling, renamed into place. Each write also names the digest of the record it replaces, or its absence, and is refused if the file changed in the meantime. That digest is the BLAKE3 hash of the record file's bytes. This is the same optimistic rule the commit gate applies to stale work. A short advisory lock on `.conseqa/local/conformance.lock` makes the comparison and the rename one step across processes. That file is machine-local and outside version control, and the lock is never the spec writer lock. Hand edits are allowed and are authoritative on read, since the files are, as in the spec. A hand-written record with wrong digests simply reads as `drifted`.

## 8.5 Merging

One file per declaration means two branches conflict only when both checked the same declaration, which is a real conflict. A merged tree's state is derived like any other (§13.1).

## 8.6 Version axes

A record of another `format`, `dsl`, or `fact_format`, other than one `FACT_READS` admits, is not read as evidence. Its declaration reads as `unattested`, with the reason naming the record and both versions. A DSL bump therefore leaves every declaration unattested instead of failing the whole derivation. That is the honest outcome: the declarations' meaning changed, so every check is owed again. A record missing `format`, `dsl`, or `fact_format` is not of another version but malformed, and is refused (§8.1).

---

# 9. Completeness claims

## 9.1 Why

Some facts no region can attest. `no_deleters(object.order)` is true of the code only if no code deletes orders outside the transactions the model declares. `conflict_closure` and `cyclic_components` are true of the code only if no unmodeled code writes the objects involved, since an unmodeled writer joins the real conflict closure invisibly. These facts need a statement about the whole repository. Without one, a single unbound `DELETE FROM orders` in a maintenance script silently falsifies a proven property.

## 9.2 The claim

A `data_object` record may carry `writers` completeness. A `topic` or `outbox` record may carry `publishers` completeness:

```yaml
completeness:
  - scope: writers
    patterns:
      - '(?i)\b(update|delete\s+from|insert\s+into)\s+orders\b'
      - '\bOrderRepo::(save|delete)\b'
    exceptions:
      - path: tools/backfill_versions.rs
        reason: one-off backfill, not deployed
```

The claim reads: every match of these patterns, in the scanned files (§6.5), lies in a region bound to a transaction the model declares as writing this object, or in an excepted path. For `publishers`, the regions are those of the effect sites the model declares as publishing to, or writing into, the channel, and of the transitions whose side effects publish to it. The declared writers and publishers come from the design graph (`GraphQuery::Writers`, `GraphQuery::Publishers`).

Patterns are regular expressions in the syntax of Rust's `regex` crate. A record whose pattern does not compile is refused (§8.1). An exception names one file, exactly as the scan names it.

## 9.3 Checking

The claim is re-checked on every derivation, not trusted from the time it was written. A new match outside declared regions and exceptions turns the condition `incomplete`, and the work item lists the matches. A claim is not sound: patterns can miss. But it is recorded, reviewable, and continuously re-checked, which is the most a language-agnostic tool can offer. Reviews (§14.3) are where the patterns themselves get scrutinized.

How a match is placed:

- Each pattern runs over a scanned file's whole text, as written, so a match may span lines, as a SQL statement often does.
- A match of no text touches no line, and is passed over.
- A match lies in a region when every line it touches is strictly between the region's markers. A marker line is text like any other, and is not inside its own region.
- A file with a scan error contributes no regions (§6.6), so its matches lie outside.
- A status lists the first 20 matches outside each claim, and counts the rest.

## 9.4 When a claim is required

A model-wide fact in the current reliance requires these claims:

| Fact | Requires |
|---|---|
| `no_deleters` | `writers` completeness of its object |
| `conflict_closure`, `cyclic_components` | `writers` completeness of every object the closure's transactions read, write, or lock |

`message_identity` facts also require `publishers` completeness of their channel. An unbound publisher could put a second message with an existing identity on the channel.

A required scope holds when the carrier's readable record carries at least one claim of that scope and every one of them holds. With no readable record, the claim is absent. A claim no proof requires is neither checked nor reported.

---

# 10. Conformance state

## 10.1 Conditions

Each bindable declaration has a set of conditions, possibly empty:

| Condition | Holds when |
|---|---|
| `unbound` | The declaration has no region, and its kind is one the policy requires bound (§14.4). |
| `orphaned` | A region's key, or a record, names no declaration of the current spec. Reported against the key. |
| `unattested` | The declaration is bound, but no readable record exists (§8.6). A record set aside is named, with its reason. |
| `stale` | The record's residual differs from the current residual, or, for a realized declaration, its `realization` differs from the chosen one (§7.3). |
| `drifted` | The record's regions differ from the current regions, compared as a multiset of digests. Editing, adding, or removing a region drifts the record. Paths and lines are presentation, so moving a region unchanged does not. |
| `missing_facts` | A required attestable fact is not attested in the record, or a required structural fact (§7.3) lacks the record's `structural: true`. With no readable record, nothing is attested. |
| `incomplete` | A required completeness claim is absent, or does not hold (§9.3, §9.4). With no readable record, every required claim is absent. |
| `surplus` | The record attests a fact that is no longer required. |
| `unreviewed` | The policy requires a review of something the record carries (§14.4), and no review by another party confirms it at the current digests (§14.3). |
| `disputed` | A review at the current digests disputes something the record carries: the record as a whole, or one of its items. A review by anyone counts, the attester included. |
| `unrealizable` | With a stack profile, the declaration is `ambiguous`, has an `invalid_choice`, or is `infeasible` (Part VI §10.3). Its work item is parked: the remedy is a decision about the spec or the stack, not code. |

A declaration with no conditions is **current**.

`missing_facts` and `incomplete` hold whatever the policy requires bound. A declaration that owes a fact, such as a `transition` implementing `retry_may_repeat`, therefore always has a condition saying so.

## 10.2 Obligation conformance

A proven obligation is **conformant** when every fact of its basis holds of the code:

- every attestable fact is attested, at current digests, by every implementer: each implementer's readable record lists it and is not `drifted`;
- every structural fact's bindable anchors claim it: each one's readable record has `structural: true` and is neither `stale` nor `drifted`;
- every model-wide fact's required claims hold;
- every `relies_on` obligation is itself conformant.

An obligation can therefore be proven in the model and not conformant in the code, which is the distinction this revision exists to draw. `conseqa conform status` reports, for every proven obligation, whether it is conformant, and if not, which declarations block it.

Reviews and disputes are not part of conformance. They say who else checked a record, not what it records, and they are gated apart, as `--fail-on not_conformant,disputed` does (§15.3).

A non-conformant obligation names its own blockers: each declaration whose condition keeps a fact of its own basis from holding, with the condition and the facts. Under `through`, it names the obligations it relies on that are not conformant. Relied-on obligations are read as proofs read them, coinductively: obligations relying on each other in a cycle are conformant unless something in the cycle fails.

## 10.3 Summary

The status summary counts declarations by condition, and proven obligations by conformance:

```text
declarations   41 bindable: 30 current, 6 stale, 2 drifted, 3 unbound
obligations    14 proven: 9 conformant, 5 not conformant
  ! SerializableBy(order_id) on tx.apply_payment
      missing_facts  transaction operation.apply_payment/tx.apply_payment
                     guarded_mutation(object.order, compare_and_set, observed_version)
  ! Recoverability(event_id), completion: guaranteed on operation.ship
      drifted        subscription_runtime operation.ship/input.ship.paid   infra/queues.tf
```

---

# 11. The work list

## 11.1 Items

There is one work item per declaration that has conditions, and one per orphaned key. An item carries all of its declaration's conditions, because an agent changing a declaration's code should handle everything wrong with it at once.

## 11.2 Identity

An item's id is its declaration's textual key. The same item has the same id on every derivation until its conditions clear, on every machine and every branch.

## 11.3 Priority

1. **Correctness.** The item blocks the conformance of a proven obligation: one of its conditions involves a fact or claim in that obligation's basis.
2. **Structure.** Items for `unbound`, `unattested`, `stale`, or `drifted` declarations that block no proven obligation.
3. **Review.** Items whose only conditions are `unreviewed` or `disputed`.
4. **Cleanup.** `orphaned` keys and `surplus` facts. This is optional work. A surplus fact is one no proof needs any longer, which is not the same as one the code may stop providing (Part I §13.2).

## 11.4 Order within a priority

Items follow dependency order:

1. `schema`, `data_model`, `data_object`, `outbox`, `topic`;
2. `transaction`, `effect_site`;
3. `program`, `input`;
4. the L1 kinds;
5. otherwise by key.

Within the second group, callees come before callers (`GraphQuery::Callees`). Implementing a caller before the callee it relies on produces code that has nothing correct to call.

## 11.5 Scoping

`implementation_plan --base <ref>` (and `conseqa conform plan <base>`) keeps only the items whose declarations Part I's diff from `<base>` reports as changed, plus the items anchoring a fact of an obligation the diff reports as `regressed`, `improved`, or `rebased`. This is the view for a pull request that changes spec and code together: the code work that this branch's spec changes owe.

## 11.6 The spec may lead the code

A non-empty work list is a normal state. A spec change can land first, and its implementation can follow over several pull requests. What CI refuses is policy, chosen per branch through `conseqa conform check --fail-on` (§15.3), not a property of the tool.

---

# 12. Work items

## 12.1 Contents

The packet for one item holds everything an agent needs to do it, and nothing it can fetch for itself.

| Section | Contents |
|---|---|
| **Declaration** | The textual key and the current residual (canonical YAML). When a record exists, the structural delta from the recorded residual to the current one (Part I §8.2). |
| **Conditions** | Each condition, stated in a sentence. |
| **Required facts** | Each fact's canonical form and a one-line rendering. Also the obligations whose basis contains the fact, by display name, with each obligation's summary line, and the load-bearing check (§12.2). |
| **Completeness** | The required claims, their current matches outside declared regions, and the declared writers or publishers. |
| **Code** | The item's regions (path and line range). The regions of its neighbours one hop away in the design graph: callers and callees, readers and writers of the objects it touches, publishers and consumers of its channels. Paths and ranges only, since the agent reads the files itself. |
| **Realization** | With a stack profile (Part VI):<ul><li>the chosen catalogue entry, and why: automatic, or the architect's recorded reason;</li><li>each realization requirement on this declaration, with its statement and evidence shape;</li><li>the requirements this declaration's realization places on its neighbours, so a topic's implementer knows that producers must set the record key;</li><li>the idiom entries matching the project's frameworks for the primitives the declaration uses.</li></ul>This is the guidance an agent needs before any precedent exists. |
| **Precedents** | For each required fact kind, how this project already implements it (§12.3). |
| **Semantics** | For each primitive the declaration uses, the guide topic that defines it, by the name the design MCP `dsl_guide` tool resolves (for example `Atomic conditional mutations`, `Locks`, `Outbox write effect`). |
| **Done when** | The checklist: regions bound with markers; structural conformance; each required fact evidenced; each required claim holding; then `attest`; then, where the policy requires it, a review. |
| **Read set** | The declarations and graph queries the packet was built from, with their digests (§13.2). |

## 12.2 The load-bearing check

For every required fact whose kind has a withdrawal mutation in Part I's mutation catalogue (§19.2), the packet says what the withdrawal costs. It applies that mutation to the current model, verifies the result, and lists the obligations that regress:

```text
guarded_mutation(object.order, compare_and_set, observed_version)
  without it: SerializableBy(order_id) on tx.apply_payment   proven -> unknown
              SerializableBy(order_id) on tx.cancel_order    proven -> unknown
```

The withdrawals at this revision:

| Fact | Withdrawal |
|---|---|
| `isolation` | set to `unspecified` |
| `lock` | remove the lock step |
| `guarded_mutation` | drop the comparison: a `compare_and_set` becomes an `update`. A transition, cursor advance, fence, or upsert has no comparison whose removal withdraws the fact, so it falls back as a kind without a withdrawal does. |
| `deduplicated_by` | remove it |
| `delivery` | set to `unspecified` |
| `external_guarantee` | set the guarantee to `unspecified` |
| `retry_may_repeat` | set the retry to `unspecified` |
| `message_identity` | set the identity to `unspecified` |

A kind without a withdrawal falls back to listing the obligations that rely on the fact. A `realization` fact has no proof cost, so the packet states instead the capability its requirement secures: "without it, `topic_runtime topic.payment_captured` no longer provides `ordering: within_group` as the spec declares". Models are small and verification is fast, so the check runs on every packet. This is the sentence that stops an agent from deleting the `version = $2`: not a rule that it should be careful, but the named properties it would break.

## 12.3 Precedents

For each required fact kind, the packet cites up to three evidence spans from other records that are current, have no conditions, and are confirmed by a review. Records about the same object, then the same data model, come first. A new transaction's packet can then say: this project implements an observed-version `compare_and_set` like `src/orders/repo.rs:88–95`. Unreviewed evidence is never a precedent, because an unchecked idiom spreading through the codebase is worse than none.

## 12.4 Size and format

A packet references code by span and never embeds file content. It embeds only the residuals and deltas, which are bounded by one declaration. Neighbour lists are capped at 20 per relation, with the count of the rest. The MCP tool returns the packet as JSON with a markdown rendering in front, and the CLI prints the markdown, or the JSON with `--json`.

---

# 13. Tracking the spec across versions

## 13.1 Derivation

Every condition, item, and packet is derived from four inputs read at one moment:

- the spec files (through the store loader);
- the scanned files;
- the records;
- the policy.

Nothing is derived from history, and nothing persists between derivations. The answer for a branch is the answer for its files.

## 13.2 Work in flight

An agent works an item over many steps, and the spec can move meanwhile: another agent's design commit, a `git pull`, a teammate's merge. Each packet carries its read set: the residual digests of the declarations it was built from, and the result digests of the graph queries it used. This mirrors the design engine's `TaskReadSet`.

`attest` takes the packet's read set and re-derives. If a member changed, attest is refused as stale, naming the changes, by the same rule `stale_causes` applies to design tasks (`src/design/invalidation.rs`). The agent fetches a fresh packet, sees the delta, and adjusts. Work done against a spec that has since moved is never recorded as conforming to it.

## 13.3 Relation to Part I's diff

The delta in a packet is Part I's structural delta between the recorded residual and the current one, so no base commit is involved. Each record is its own baseline (§3.2). Part I's diff between commits is used only for scoping (§11.5), and by reviewers reading a pull request.

---

# 14. Attesting and reviewing

## 14.1 Attest

The caller supplies:

- the declaration key;
- the facts it attests, each with evidence spans;
- `structural: true` or `false`;
- any completeness claims;
- `by`;
- the packet's read set (optional, but always sent by the MCP tool's documented flow).

The caller never supplies regions, digests, or the residual.

## 14.2 Validation

Attest refuses, by name, unless all of the following hold:

1. no file holding the declaration's regions or the evidence has a scan error;
2. the declaration exists, is bindable, and is bound;
3. every fact is currently required (§7.3), with this declaration among its implementers;
4. every evidence span lies in a region bound to the implementer or to one of the fact's anchors (§7.4);
5. every completeness pattern compiles, and the claim holds now (§9.3);
6. the read set, if given, is not stale (§13.2);
7. the record being replaced is the one the caller read (§8.4).

The tool then computes the regions and digests from the current files, snapshots the current residual, and writes the record. Reviews whose digests no longer match are dropped. A review covers the code it saw, not code written since.

Partial attestation is allowed. Attesting two of three required facts leaves `missing_facts` for the third.

## 14.3 Reviews

A review is a second party's check of a record against the code. It gives a verdict, `confirmed` or `disputed`, for the record as a whole and optionally per fact and per claim, with notes. It records the digests it saw. It applies while they are the record's region digests, counted as a multiset, so it lapses when a region is edited, added, or removed.

An item's verdict overrides the review's verdict on the record as a whole, for the item's target. A review confirms one thing the record carries when it saw the current digests, is by another party than `attested.by` (§14.5), and its verdict on that thing is `confirmed`. A checker review satisfies only the realization requirements its checker covers (below), so until catalogue checkers exist (Part VI §13) it satisfies no required review.

`conseqa conform review [<key>…] --backend claude|codex` runs one fresh-context worker per record, through the harness backends `conseqa design` already uses, with their scratch configuration outside the repository. A worker receives the record, the item's packet, and read access to the files, but not the attester's reasoning. It reports through the review operation. Done this way, a review is an independent read, not the attester's argument repeated back.

**Checker reviews.** A catalogue entry's deterministic checkers (Part VI §13) run on every derivation, over the regions bound to the declaration their requirement applies to. Each result is recorded as a review, `by: { kind: checker, name: <entry id>/<checker id>@<entry version> }`, at the digests it read. Like any review, it lapses when they change.

## 14.4 Policy

```yaml
# .conseqa/conformance/policy.yaml
format: 1
require_binding: [program, transaction, effect_site, data_object, outbox, topic,
                  subscription_runtime, outbox_runtime]
review:
  relied_on_facts: required     # required | optional
  completeness: required
  structural: optional
  realization: required
  checker_satisfies: [realization]   # fact kinds a confirming checker review satisfies
```

When the file is absent, these values are the defaults. When it is present, `format` is required, and any other field it omits takes its default. A kind in `require_binding` that is not bindable is refused, and so is any kind but `realization` in `checker_satisfies`. A fact or claim that no proof relies on needs a review only when it is a `realization` fact.

What needs a review, then, is what proofs rely on:

- each fact the record attests that is required of it (`relied_on_facts`);
- each claim of a scope the reliance requires of it (`completeness`);
- its structural claim, when a relied-on structural fact rests on it (`structural`).

A surplus fact, or a claim no proof needs, needs none.

When the project has a stack profile, every declaration with a chosen realization must also be bound, whatever `require_binding` lists. This is what brings routers, execution pools, storage layouts and topic runtimes under conformance: the spec's topology is held to what it declares, even where no proof relies on it.

A confirming checker review satisfies the required review of exactly the requirements its checker covers. At this revision `checker_satisfies` may list only `realization`, because no catalogue checker covers a proof fact. A checker can confirm that a Terraform topic exists; it cannot confirm that a conditional update guards what a proof needs.

## 14.5 Provenance

`by` is a claim. Conseqa records it and cannot authenticate it: the pull request that commits a record is the trust boundary, as it is for the spec itself. Two rules keep the claim honest within the tool:

- a review whose `by` equals the record's `attested.by` does not satisfy a required review;
- the tool computes everything it can, so a record can be wrong only in its judgments, never in what code it describes.

---

# 15. Surfaces

## 15.1 Design MCP tools

| Tool | Served to | Does |
|---|---|---|
| `conformance_status` | every role | The summary of §10.3, with each declaration's conditions and each proven obligation's conformance. |
| `implementation_plan` | every role | The ordered work list. `base` and `merge_base` scope it as in §11.5. |
| `work_item` | every role | The packet for one item, by key. |
| `attest` | the coordinator | §14.1–§14.2. |
| `record_review` | the coordinator | Records one review of one record. |

The read tools join the shared router. The write tools join the coordinator router, which serves the stdio client, the API key, and session tokens. A design-task worker's job is a design task, so it is not served the write tools. The tools read the spec through the engine's head, which already adopts outside edits before every call. Records are written outside the spec, so attesting never contends with the commit gate.

## 15.2 CLI

```text
conseqa conform status [--json] [--report <path>]
conseqa conform plan [<base>] [--merge-base] [--json]
conseqa conform item <declaration key> [--json]
conseqa conform attest --input <path>
conseqa conform review [<declaration key>…] --backend claude|codex
conseqa conform scan
conseqa conform check [--fail-on <condition>[,<condition>…]] [--report <path>]
```

`attest --input` takes the same JSON the MCP tool takes. Like every `conseqa` subcommand, `conform` discovers the project from the working directory. It takes no spec writer lock, so it runs beside an open design MCP server, and §8.4's optimistic replace keeps two writers from losing each other's records.

## 15.3 Gates and exit codes

`conseqa conform check` always fails on scan errors. `--fail-on` adds conditions, by their names in §10.1, plus `not_conformant`, which fails when any proven obligation is not conformant. A deploy branch's CI might run:

```text
conseqa conform check --fail-on not_conformant,disputed
```

A feature branch might gate on scan errors alone, letting the spec lead.

| Exit | Meaning |
|---|---|
| 0 | Derived, and no gate failed. |
| 1 | Could not derive: the spec, the records, or the policy file did not load (§8.1), the spec does not assemble and validate (it then has no proofs, and nothing is required of its code), or git failed. |
| 2 | A gate failed, or there is a scan error. |

---

# 16. Relationship to the other parts

- **Part I** provides:
  - residuals and structural deltas, for `stale` and for packets;
  - obligation keys, for naming the obligations that rely on a fact;
  - the basis and durable facts, which records store;
  - the reliance, for required facts;
  - the diff, for scoping;
  - the mutation catalogue, for the load-bearing check.

  This document supersedes the Part IV outline in Part I §21.
- **Part II, deployment baseline.** "What does production implement" becomes the records and code at the deployed commit. "Is production conformant" is `conseqa conform check --fail-on not_conformant` run at that commit. The baseline Part II needed is therefore already in the repository, per declaration.
- **Part III, logical migrations.** A data-model change's migration steps become work items on `data_object` regions, ordered by the migration plan's expand / migrate / contract sequence.
- **Part V, mixed-version verification.** Each running version's records describe what its code actually does, which is the input a transition model needs about the version being replaced.

---

# 17. Implementation changes by module

## 17.1 `src/conformance/` (new, `design` feature)

| Module | Holds |
|---|---|
| `mod.rs` | The conformance format, digests, the bindable kinds, and the errors the modules share. |
| `markers.rs` | The scan, marker parsing, regions, region residuals and digests, and diagnostics (§6). |
| `record.rs` | The record format, the strict reader, atomic and optimistic writes, and the version axes (§8). |
| `policy.rs` | The policy file and its defaults (§14.4). |
| `completeness.rs` | Claims and their re-check (§9). |
| `state.rs` | Required facts, conditions, obligation conformance, and the summary (§7.3, §10). |
| `plan.rs` | The work list, priorities, order, and scoping (§11). |
| `packet.rs` | Work items, the load-bearing check, and precedents (§12). |
| `attest.rs` | Validation and writing (§14.1–§14.2). |

## 17.2 `src/analyzer/basis.rs` (Part I)

Each fact kind gains its conformance class and the rule naming its implementers (§7.1, §7.2). Most facts name their implementer in their own fields. A cursor's, a fence's, and a message identity's implementers are every transaction or producer the model declares doing the work, so the conformance module resolves those rules against the design graph and the workspace.

## 17.3 `src/design/mcp.rs`

The five tools of §15.1: reads in the shared router, writes in a new `conformance_tools` router joined to the coordinator and session routers.

## 17.4 `src/main.rs`

The `conform` subcommand group (§15.2).

## 17.5 `src/harness/`

A review workflow that runs fresh-context workers through the existing backends and reports through the review operation (§14.3).

## 17.6 `docs/live/PROJECT_STATE.md`

`.conseqa/conformance/` in the layout, and that the store neither loads nor scans it.

---

# 18. Test plan

## 18.1 Markers

Cover:

- every comment leader of §6.1;
- trailing closers;
- a mismatched end key;
- a begin without an end;
- overlap;
- nesting;
- CRLF files;
- a non-UTF-8 file skipped;
- an orphaned key reported as a condition, not a scan error;
- `.conseqa/` never scanned;
- untracked-but-not-ignored files included and ignored files excluded.

## 18.2 Digests

- Editing a nested region drifts only the inner declaration.
- Editing the outer region's own lines drifts only the outer one.
- Editing outside every region drifts nothing.
- Normalizing line endings drifts nothing.

## 18.3 Records

Cover:

- round-trip;
- strict directories and file names;
- an optimistic replace refused when the file changed;
- a record of another `dsl` or `fact_format` reading as `unattested`, with its reason;
- a hand-edited record adopted;
- reviews dropped when digests change.

## 18.4 Required facts and obligation conformance

On a fixture whose serializability proof uses an observed-version `compare_and_set`:

1. Bind and attest the guarded mutation with evidence. The transaction is current and the obligation conformant.
2. Change the spec to serializable isolation. The transaction is `stale` and `missing_facts` (isolation) and `surplus` (the guarded mutation). The obligation is proven, not conformant, until re-attested.

## 18.5 Attest refusals

Each clause of §14.2 refused by name:

- a fact that is not required, or required of another implementer;
- evidence outside every allowed region;
- an unbound declaration;
- a stale read set;
- a scan error in a file involved;
- a replaced record that changed.

## 18.6 Completeness

`no_deleters` with `writers` patterns:

- a new match outside declared writers makes it `incomplete`, listing the match;
- an exception restores it;
- a match inside a declared writer's region is accepted.

## 18.7 Work list

On the fixtures, check:

- priorities and dependency order;
- callees before callers;
- ids stable across derivations;
- `--base` scoping equal to Part I's changed declarations plus the anchors of affected obligations.

## 18.8 Packets

A packet contains the delta from the recorded residual, the required facts with their relying obligations, and the load-bearing check for each catalogued kind, matching an independent verification of the withdrawn model. Its precedents come only from current, reviewed records. Every semantics topic resolves through the `dsl_guide` lookup.

## 18.9 Reviews

- A review lapses on a digest change.
- An attester reviewing itself does not satisfy a required review.
- A disputed review raises `disputed`.

## 18.10 Determinism

The same inputs produce byte-identical status, plan, and packet JSON.

## 18.11 CLI

Each exit code and gate of §15.3.

## 18.12 Realization requirements

With a stack profile, covered in detail by Part VI §20:

- realization facts appear as required facts on their declarations;
- a changed chosen entry makes its records `stale`;
- an unrealizable declaration's item is parked;
- checker reviews satisfy only the requirements their checkers cover.

---

# 19. Acceptance criteria

1. Markers bind regions of any text file to declarations, and every malformed marker is a named scan error.
2. Region digests follow nesting, so each region drifts only for its own lines.
3. Records are committed per declaration, store the residual they were checked against, and are refused or degraded by name across version axes, never misread.
4. Attest computes regions, digests, and residuals itself, attests only required facts with evidence in allowed regions, and refuses stale work.
5. Every proven obligation is reported as conformant or not, with the declarations blocking it.
6. Model-wide facts require completeness claims, which are re-checked on every derivation.
7. The work list is a pure function of the files: prioritized, dependency-ordered, with stable item ids, and scopable by Part I's diff.
8. Every packet names its load-bearing facts with the obligations each one carries, the withdrawal's cost where catalogued, and reviewed precedents.
9. Reviews run in fresh context through the existing backends, are bound to digests, and never count when given by the attester.
10. The MCP tools and the CLI serve the same derivation, and the store format, the DSL, and every verdict are unchanged.
11. With a stack profile, realization requirements are required facts, every realized declaration is bound, and catalogue checkers record reviews that satisfy only what they cover.

---

# 20. Decisions taken, and alternatives

1. **Who may attest.** Anyone may attest. By default, facts a proof relies on and completeness claims also need an independent review, by a fresh-context agent or a person. *Alternatives:* self-attestation only (weak evidence, for the reason any author's review of their own work is weak); human review only (slow at the rate agents write code); deterministic checkers (§21, the right long-term answer for fact kinds with a recognizable code shape).
2. **Region extent.** Explicit begin and end markers. *Alternative:* one marker binding the next function or class through a parser such as tree-sitter. That is shorter to write, but needs a grammar per language, and fails exactly where conformance matters most: SQL migrations, Terraform, YAML configuration. It can be added later as a second marker form without changing records.
3. **Facts outside code.** Any text file can carry markers, so database configuration, migrations, and Terraform are bound like code. *Alternative:* resource tags on deployed infrastructure. Deferred, since querying deployed infrastructure is runtime inspection (§4).
4. **The name.** `conseqa conform`, and "conformance" for this feature, because the semantics contract already uses the word for exactly this (§1.3: "conditional on model conformance"). `conseqa-design-mcp decider conformance` keeps its name: it lives on the other binary and vets a System One wire server, which is unrelated. Renaming it to `decider vet` is a separate, optional change if the overlap confuses.
5. **Where records live.** In `.conseqa/conformance/`, so a project's Conseqa state stays in one directory, but outside the store's load and scan, so attesting never moves the spec revision. *Alternative:* records beside the code they describe. Rejected because it scatters the state and makes "list every record" a repository walk.
6. **One record per declaration.** Merge conflicts happen only where two branches checked the same declaration. *Alternative:* one project-wide file, which would conflict on nearly every pull request.
7. **Only required facts are attested.** A record is about what the proofs need. Structural conformance covers the rest of the declaration's content.

---

# 21. Open questions

- **Readers completeness.** A `writers` claim does not cover unmodeled readers. An unbound read-only path can still observe a non-serializable state. Whether read-side completeness is worth its cost is undecided.
- **Deterministic checkers.** Partly resolved by Part VI §13. Catalogue entries ship checkers for realization requirements with a known shape: a Terraform resource, an Envoy cluster field, a DDL clause. Checkers for proof facts in application code remain open: an `UPDATE … WHERE <version> = ?` for an observed-version guard, or an isolation level in a `BEGIN`.
- **Generated code.** Markers in generated files vanish on regeneration. The working rule is to bind the generator's input, such as the migration source or the ORM schema. Whether the tool should know about generators is open.
- **Several repositories.** A spec in one repository implemented in others is out of scope. Markers and records live in the project's repository.
- **Precedent quality over time.** Precedents are only as good as the reviews behind them. Whether precedents should also prefer recent records is open.

---

# 22. Documentation reconciliation

When this part is implemented:

- `docs/live/` gains `CONFORMANCE.md`, describing markers, records, completeness, state, the work list, packets, reviews, and the surfaces against the implementation, and `docs/live/README.md` lists it;
- `PROJECT_STATE.md` adds `conformance/` to the layout and the `conform` subcommands to its table of surfaces;
- §25 of `CONSEQA_DSL_SEMANTICS.md` gains a sentence. A proof's conformance assumption becomes checkable per obligation, and "proven" and "conformant" are distinct, reported states;
- this document then becomes a historical design record.
