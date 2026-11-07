# Conformance

Every Conseqa proof is conditional on the code conforming to the declarations it consumed (§25 of the semantics contract). Conformance makes that condition checkable, per declaration and per obligation, without parsing any language:

1. **binding markers** bind regions of any text file to declarations;
2. **conformance records**, one committed file per declaration, state what its regions were checked to provide;
3. **completeness claims** state that no unbound code touches what a model-wide fact depends on, and are re-checked on every derivation;
4. every declaration's **conditions**, and every proven obligation's **conformance**, are derived from the spec, the files, the records, and the policy;
5. the **work list** and **work items** turn what the code owes into ordered, self-contained tasks for an agent;
6. **attest** records what an agent checked, and **reviews** record a second party's check.

This document describes the implementation. The design record is `Conseqa_Conformance_and_Implementation_Guidance_Revision_Specification.md` (Part IV), now historical. Where they differ, the code and its tests win (see the README's precedence rule). Nothing here changes a verdict: conformance reads proofs, it never makes them. "Proven" and "conformant" are distinct, reported states: an obligation can be proven of the model and not conformant in the code.

With a stack profile, conformance also holds the code to what the chosen realizations demand of it (Part VI): `realization` facts are required facts, every realized declaration is bound, a record names the realization its code was checked against, a declaration the stack cannot realize is `unrealizable`, a work item carries a Realization section, and catalogue checkers review what they cover. `REALIZATION.md` describes them; their place here is noted below.

## Surfaces

### `conseqa conform`

Like every `conseqa` subcommand, `conform` discovers the project from the working directory. It reads the spec from the project's files and takes no writer lock, so it runs beside an open design MCP server. Every subcommand's `--json` prints its conformance document (see [Documents](#documents)).

| Command | What it does |
|---|---|
| `conform status [--json] [--report <path>]` | The summary: declarations by condition, orphaned keys, scan errors, and every proven obligation's conformance with what blocks it. `--report` writes the status document. |
| `conform plan [<base>] [--merge-base] [--spec-range <a>..<b>] [--json]` | The work list. `<base>` scopes it to what the spec's changes from that commit to the working tree owe; `--merge-base` reads the base at its merge base; `--spec-range` scopes it across two commits instead. |
| `conform item <key> [--json]` | The work item for one declaration, or an orphaned key, in markdown. |
| `conform attest --input <path> [--json]` | Attests from the same JSON the MCP tool takes; `-` reads stdin. Stamped with git's `user.name`. |
| `conform review [<key>…] --backend claude\|codex [--backend-program <p>] [--model <m>] [--max-agents <n>] [--json]` | Reviews records with fresh-context agents (see [Reviews](#reviews)). |
| `conform scan [--json]` | Every region and every scan error, read from the files alone, so it runs when the spec does not verify. |
| `conform check [<base>] [--merge-base] [--spec-range <a>..<b>] [--fail-on <gate>[,<gate>…]] [--report <path>] [--json]` | The CI gate (below). |

| Exit | Meaning |
|---|---|
| 0 | Derived, and nothing failed. |
| 1 | Could not derive: the spec, a record, or the policy did not load; the spec does not assemble and validate (it then has no proofs, and nothing is required of its code); git failed; or the input or the key named nothing. |
| 2 | `check`: a gate failed, or there is a scan error. `scan`: a scan error. `attest`: refused. `review`: a record was left unreviewed. |

`conform check` always fails on scan errors. `--fail-on` adds gates: the conditions by name (`unbound`, `orphaned`, `unattested`, `stale`, `drifted`, `missing_facts`, `incomplete`, `surplus`, `unreviewed`, `disputed`) and `not_conformant`, which fails when a proven obligation is not conformant. A non-empty work list is a normal state, so a feature branch can let the spec lead and a deploy branch refuse what does not conform:

```text
conseqa conform check --fail-on not_conformant,disputed
```

Given a base or a spec range, a check gates only on what that diff scopes: the conditions of the items in scope, and the conformance of the obligations the diff moved.

### The design MCP server

| Tool | Served to | Does |
|---|---|---|
| `conformance_status` | every role | The summary, then the status document. |
| `implementation_plan` | every role | The work list, then the plan document. `base` (with `merge_base`) or `spec_range` scopes it. |
| `work_item` | every role | A work item by `key`, in markdown, then the item document. |
| `attest` | coordinator, implementer | Attests; returns the attest document, an error result when refused. |
| `record_review` | coordinator, reviewer | Records one review; returns the review document, an error result when refused. |

On an engine, the tools read the spec at the engine's head, which adopts the project's files before every call. Records are written outside the spec, so a write never contends with the commit gate.

Four roles are served, each its own brief and tools (`src/design/mcp.rs`):

- the **coordinator** (the stdio client, the API key, an interactive session's token) authors the spec and may attest and review;
- a design task's **worker** authors its own scope through `submit_patch`;
- an **implementer** reads and attests;
- a **reviewer** reads and records reviews.

Neither an implementer nor a reviewer is served `submit_patch` or `build_design`: they change code and records, never the spec. Their tokens are minted on a single-project server's localhost admin surface, as task and session tokens are:

```text
POST /admin/credentials   {"role": "implementer" | "reviewer",
                           "by": {"kind": "agent" | "person", "name": "<provider/model>", "job": "<id>"}}
```

Each token is a read-only session of the engine carrying its role and `by`. The global and stdio servers serve the coordinator alone.

**Who attests and reviews is never the caller's to say.** An attest's or a review's input names no `by` (a tool call naming one is refused at parse). A role token stamps its writes with the `by` it was minted for. The coordinator's surfaces and the CLI stamp `{kind: person, name: <git config user.name>}`, refused while git names no one. A record's `by.job` is provenance: the rule that a review by the attester satisfies no required review compares kind and name, so an agent cannot review its own work from a second job.

## Markers and regions

`src/conformance/markers.rs`.

```rust
// conseqa:begin transaction operation.apply_payment/tx.apply_payment
…
// conseqa:end transaction operation.apply_payment/tx.apply_payment
```

A marker is the keyword `conseqa:begin` or `conseqa:end` after any comment leader, whitespace, the declaration's textual key (exactly one space between its kind and its ids), then nothing but whitespace and one optional closer (`*/` or `-->`), which may touch the key. A region is the lines strictly between a begin and its end, in one file. An end closes the innermost open region with its key's text; matching an outer one is an overlap, and matching none is a mismatched end.

A region's digest is BLAKE3 over its text, with line endings normalized to `\n` and each directly nested region, markers included, replaced by one line holding exactly its key. Editing a transaction nested in a program's region drifts the transaction, not the program, and re-indenting the markers drifts neither. A region's `lines` are its markers' line numbers: presentation, since records rest on digests.

The scan reads the project directory's files that git tracks, and the untracked ones it does not ignore; outside git, it walks the directory, skipping dot-directories. It never reads `.conseqa/`, a symbolic link, a submodule, a tracked file missing from the working tree, or a file or path that is not UTF-8. Scan errors are named by file and line: a malformed marker, a key that does not parse, a begin without an end or an end without a begin, an end whose key differs from its begin's, overlapping regions, and a key naming a kind that is not bindable. A file with a scan error contributes no regions. A key naming no declaration of the current spec is no scan error but an `orphaned` condition.

Every member of the declaration partition is bindable except a `requirement`, which proofs discharge, and a `prompt_obligation`, which is intent.

## Records and the policy

`src/conformance/record.rs`, `src/conformance/policy.rs`.

A record lives at its declaration's textual key below `.conseqa/conformance/`: the kind as a directory, then each escaped id as a path segment, the last with `.yaml`. The store neither loads nor scans the directory, so writing a record never moves the spec revision, invalidates a design task, or takes the writer lock.

```yaml
format: 1                 # conformance format: records and the policy
dsl: 6
fact_format: 1
declaration: transaction operation.apply_payment/tx.apply_payment
residual: { … }           # the declaration's residual when checked
regions:                  # computed by the tool
  - { path: src/payments/apply.rs, lines: [42, 71], digest: 9f2c… }
structural: true          # its regions implement the residual and nothing beyond
facts:                    # each in its canonical durable form, with evidence
  - fact: { kind: guarded_mutation, … }
    evidence: [{ path: src/payments/apply.rs, lines: [47, 51] }]
completeness: []          # data_object, topic, and outbox records only
# realization: { entry: kafka.keyed-topic@1, options: [] }   # a realized declaration's record only
attested:
  by: { kind: agent, name: claude/claude-opus-5-5, job: job-17 }
  commit: 8883890…        # the commit the attester's tree was based on
reviews:
  - by: { kind: agent, name: codex }
    digests: [9f2c…]      # the region digests it saw
    verdict: confirmed    # confirmed | disputed
    items: [{ fact: 0, verdict: confirmed, note: "WHERE version = $2 at 50" }]
```

The reader is as strict as the store's: an unexpected entry, a file name that is not an id's canonical encoding, an unknown field, a fact not in canonical form, a pattern that does not compile, or a missing version field fails the read, naming the file. A record of another `format`, `dsl`, or `fact_format` is set aside unread, and its declaration reads as `unattested` with the reason. Writes are atomic (a temporary sibling renamed into place) and optimistic: each names the digest of the record it replaces, or its absence, and is refused if the file changed, under a short lock on `.conseqa/local/conformance.lock`. Hand edits are authoritative on read; a hand-written record with wrong digests reads as drifted.

`.conseqa/conformance/policy.yaml` is optional. Present, it requires `format: 1`, and every other field takes its default:

```yaml
format: 1
require_binding: [program, transaction, effect_site, data_object, outbox, topic,
                  subscription_runtime, outbox_runtime]
review:
  relied_on_facts: required     # required | optional
  completeness: required
  structural: optional
  realization: required
  checker_satisfies: [realization]
```

## Required facts and completeness

`src/conformance/state.rs`, `src/conformance/completeness.rs`, and each fact kind's conformance class and implementers in `src/analyzer/basis/`.

What the code must provide is the current proofs' reliance: every fact of a proven obligation's basis.

| Class | Kinds | Provided by |
|---|---|---|
| attestable | `isolation`, `lock`, `guarded_mutation`, `cursor`, `fence`, `deduplicated_by`, `delivery`, `message_identity`, `outbox_redrive`, `retry_may_repeat`, `external_guarantee`, `propagation`, `realization` | each implementer's record attesting it, with evidence |
| structural | `naturally_replayable`, `artifact_route`, `admits_no_messages`, `no_admitted_paths`, `returns_no_result`, `terminal`, `stable_root` | each bindable anchor's record claiming `structural: true` |
| model-wide | `conflict_closure`, `cyclic_components`, `no_deleters` | completeness claims |
| internal | `relies_on`, `keyed_requirement` | nothing: a relied-on obligation conforms when it does |

An attestable fact's implementers: the transaction for `isolation`, `lock`, `guarded_mutation`, and `deduplicated_by`; every transaction with an `advance_cursor` or a `fence` step on the field for `cursor` and `fence`; the subscription runtime for `delivery`; every effect site and transition publishing the schema to the channel for `message_identity`; the outbox for `outbox_redrive`; the effect site or transition for `retry_may_repeat` and `propagation`; the effect site for `external_guarantee`; the declaration it names for `realization`. With a stack profile, every realization requirement in force is a required fact of the declaration it lands on, relied on by no proof (`REALIZATION.md`).

A claim reads: every match of its patterns in the scanned files lies in a region of a declared writer or publisher, or in an excepted file. `writers` claims go on data objects and accept the transactions the design graph declares writing the object; `publishers` claims go on topics and outboxes and accept the effect sites and transitions publishing to, or writing into, the channel. Patterns are Rust `regex`, run over each file's whole text; a non-empty match lies in a region when every line it touches is strictly between the region's markers. Required scopes: `no_deleters`, `writers` of its object; a conflict closure or its cyclic components, `writers` of every object their transactions read, write, or lock; a message identity, `publishers` of its channel. A required scope holds when the carrier's readable record carries at least one claim of it and every one holds; a claim no proof requires is not checked.

## Conformance state

`src/conformance/state.rs`. `conseqa::conformance::state::derive(project_dir, workspace)`.

Each bindable declaration has conditions, in this order; one with none is current:

| Condition | Holds when |
|---|---|
| `unbound` | It has no region, and the policy requires its kind bound. |
| `orphaned` | A region's key, or a record, names no declaration of the spec. Reported against the key. |
| `unattested` | It is bound, and has no readable record. A record set aside is named, with its reason. |
| `stale` | The record's residual is not the current residual, or, with a stack profile, its `realization` is not the declaration's current chosen or automatic realization (or it names one the declaration no longer has). |
| `drifted` | The record's region digests are not the current ones, as a multiset: moving a region changes nothing, editing, adding, or removing one does. |
| `missing_facts` | A required attestable fact is not attested, or a required structural fact lacks `structural: true`. Without a readable record nothing is attested. |
| `incomplete` | A required claim is absent, or has matches outside (the first twenty are listed). |
| `surplus` | The record attests a fact no proof relies on any longer. |
| `unreviewed` | The policy requires a review of something the record carries, and no review by another party confirms it at the current digests. |
| `disputed` | A review at the current digests disputes the record, or one of its items. Anyone's counts, a catalogue checker's included. |
| `unrealizable` | With a stack profile, the declaration is ambiguous, has an invalid choice, or is infeasible. |

What needs a review: each attested fact a proof relies on, each attested `realization` fact, each claim a proof requires, and the structural claim when a relied-on structural fact rests on it. An item's verdict overrides the review's verdict on the record as a whole, for its target. A catalogue checker's confirmation, derived on every derivation and listed as the declaration's `checker_reviews`, satisfies the review of exactly the realization requirement its checker covers; its dispute raises `disputed`.

A proven obligation is **conformant** when every fact of its basis holds of the code: every implementer's record lists each attestable fact and has not drifted; every bindable anchor's record claims structural conformance and is neither stale nor drifted; every model-wide fact's required claims hold; every relied-on obligation is conformant, read coinductively, so a cycle conforms unless something in it fails. A non-conformant obligation names its blockers (declaration, condition, facts) and, under `through`, the relied-on obligations that fail. Reviews and disputes are not part of conformance; they are gated apart.

## The work list

`src/conformance/plan.rs`.

One item per declaration with conditions, carrying all of them, and one per orphaned key. An item's id is its key. An item states its priority, its conditions, the proven obligations it blocks, `depends_on`, and `files`:

- **priority**: `parked` when it is `unrealizable`, whose remedy is a decision about the spec or the stack, after every other; otherwise `correctness` when it blocks a proven obligation; otherwise its most urgent condition decides: `unbound`, `unattested`, `stale`, `drifted` are `structure`, `unreviewed` and `disputed` are `review`, `surplus` and orphaned keys are `cleanup`;
- **order** within a priority: data first (`schema`, `data_model`, `data_object`, `outbox`, `topic`, `state_machine`), then `transaction`, `effect_site`, `transition`, then `program`, `input`, `operation`, then the L1 kinds, then the rest; among transactions, effect sites, and transitions, callees before callers (an operation calls what its effect sites request and the transitions its transactions apply; call cycles come in key order); then by key;
- **`depends_on`**: the items of the declarations its node points to in the design graph, a called operation standing for its transactions and effect sites, only those earlier in that order, so the relation has no cycle;
- **`files`**: the paths of its regions, its record's regions, and its claims' outside matches, and, while unbound, its container's regions (a transaction's or effect site's program, a program's or input's operation, a data object's or outbox's data model, a transition's state machine).

A Part I diff scopes the list (`plan::Scope`): from a base to the working tree, or across a spec range. An item is in scope when the diff changed its declaration, or when it anchors, implements, or carries a claim for a fact of either side's basis of an obligation the diff reports added, removed, regressed, improved, or rebased. An added requirement changes only the requirement, so the code its proof needs is in scope through the added obligation.

## Work items

`src/conformance/packet.rs`. There is a packet for every bindable declaration, current or not, and every orphaned key. It references code by span and never embeds a file:

| Section | Contents |
|---|---|
| Declaration | The current residual, the record's path, and the structural delta from the recorded residual. |
| Conditions | Each condition in a sentence, with what it found. |
| Disputes | Reviews at the current digests disputing the record or its items, with their notes. |
| Last refusal | Why the last attest was refused, while the record is the one that attest saw. Kept on the machine that ran it, below `.conseqa/local/conformance/refusals/`, and cleared by the next successful attest. |
| Required facts | Each fact, its class, whether the record provides it, the obligations relying on it, and the load-bearing check: the catalogue's withdrawal applied to the current model and verified, listing every obligation proven now and not after, with both statuses. A kind without a withdrawal lists the relying obligations. |
| Completeness | Each required claim, why it does not hold, and the declared writers or publishers with their regions. |
| Code | Its regions, and its neighbours' (callers and callees of its operation, readers and writers of the objects it touches, publishers and consumers of its channels), at most twenty per relation. |
| Realization | With a stack profile: the entry realizing it and why, each realization requirement on it with its evidence shape and checker findings, the requirements its realization places on its neighbours, and the idioms for the primitives it uses. A realization fact's load-bearing line names what the entry provides for the spec's needs. |
| Precedents | Per required attestable kind, up to three facts from other current records that another party confirmed at the current digests: same object first, then same data model. |
| Semantics | The `dsl_guide` topics of what it uses, each naming exactly one guide section. |
| Done when | The checklist, each step marked done or not. |
| Read set | The residual digests of the declaration and its required facts' anchors, the result digests of the design graph queries the packet ran, and the digest of the record it read. |

## Attest

`src/conformance/attest.rs`. The caller supplies the declaration, the facts with evidence spans, `structural`, any completeness claims, and the packet's read set. The tool computes the regions, digests, and residual, takes `attested.commit` from git's `HEAD`, and writes the record, facts in canonical order and spans sorted once. It refuses, by name:

| Refusal | When |
|---|---|
| `scan_error` | A file holding an evidence span, or one whose markers name the declaration, has a scan error. |
| `unknown_declaration`, `not_bindable`, `unbound` | The declaration is not one, cannot be bound, or has no region. |
| `not_required`, `not_implementer` | No proof relies on a fact, or one does but this declaration does not implement it. |
| `not_attestable` | A structural, model-wide, or internal fact. |
| `evidence_outside` | A span is not strictly inside a region of the declaration or of the fact's anchors. |
| `claim_unheld` | A given claim has matches outside, whether or not a proof requires it. |
| `stale` | A member of the read set changed, by the rule design tasks' read sets follow. |
| `record_changed` | The record replaced is not the one the read set (or, without one, the attest) read. |
| `malformed` | The input breaks the record format: a fact twice, a pattern that does not compile, a claim on the wrong kind. |

An attest that would write the record already on disk, `attested.commit` aside, writes nothing and succeeds, so a retry replays as a success. Reviews survive only an attest of exactly the same record (residual, structural claim, facts, claims) at the same digests.

## Reviews

`src/conformance/review.rs`, `src/harness/review.rs`.

A review gives `confirmed` or `disputed` for the record as a whole, and optionally per fact, per claim, and for the structural claim, with notes. The tool records the record's region digests and stamps `by`; the input names the declaration, the verdict, the items, and optionally the record digest the reviewer read. Refusals: `unknown_declaration`, `no_record` (none, or set aside), `drifted` and `stale` (the record is to be attested again first), `record_changed`, `malformed`. A party keeps one review per set of digests: a new one replaces it where it stands, and an identical one writes nothing.

`conseqa conform review` runs one fresh-context agent per record, through the backends `conseqa design` uses: the named declarations' records, or, with none named, every record with unreviewed items. Records that cannot be reviewed as they stand are reported refused and given no agent. Each agent gets a reviewer token stamped `{kind: agent, name: <backend>[/<model>], job: review-<run>-<n>}`, a strict MCP config in a scratch directory outside the repository, read-only file tools in the project, and a prompt holding the record and its work item, but none of the attester's reasoning. Its MCP server runs in the command and reads the project's files, so it needs no writer lock. The command reports what each agent recorded, and exits 2 when one recorded nothing.

## Documents

`src/conformance/document.rs`. Each document is one JSON object:

| Field | Meaning |
|---|---|
| `format` | The conformance document format, `1`, counted apart from records' and the policy's. |
| `document` | `status`, `plan`, `item`, `attest`, `review`, `scan`, or `check`. |
| `dsl`, `fact_format` | The contract its facts are written in. |

| Document | Payload |
|---|---|
| `status` | `declarations` (each `declaration`, `regions`, `required`, `claims`, `checker_reviews` when a checker ran, `conditions`), `orphaned`, `obligations` (each `obligation`, `summary`, `conformant`, `blockers`, `through`), `scan_errors`. |
| `plan` | `scope` (`base`, `head`, `changed`, `moved`) when scoped, and `items` (each `key`, `priority`, `conditions`, `blocks`, `depends_on`, `files`). |
| `item` | The packet's sections, as above. |
| `attest` | `outcome`: `written` or `unchanged` with `declaration`, `record`, `digest`, `written`, `dropped_reviews`; or `refused` with `refusal` and its fields, and `message`. |
| `review` | `reviews`, each `declaration`, `by`, and `outcome`: `recorded` or `unchanged` with `record`, `digest`, `written`, `replaced`; `refused` with `refusal` and `message`; or `not_reported` with `message`. |
| `scan` | `files` (how many were read), `regions` (each `key`, `path`, `lines`, `digest`), `errors`. |
| `check` | `scope` when scoped, `passed`, `scan_errors`, `failed` (each `gate`, and its `items` or `obligations`). |

The same inputs give a byte-identical document on any machine: every list has a defined order, and nothing records when or where it was made.

## Modules and tests

| Module | Holds |
|---|---|
| `src/conformance/mod.rs` | The conformance format, digests, the bindable kinds, and the shared errors. |
| `markers.rs`, `record.rs`, `policy.rs` | The scan and regions; records; the policy. |
| `completeness.rs`, `state.rs` | Claims and their re-check; required facts, conditions, obligation conformance, the summary, and the `Derivation` the rest build on. |
| `plan.rs`, `packet.rs` | The work list and its scope; work items and read sets. |
| `attest.rs`, `review.rs` | Attest and the last refusal; recording reviews. |
| `check.rs`, `document.rs` | Gates; the documents. |
| `src/design/mcp/conformance.rs` | The five tools and the role credentials. |
| `src/harness/review.rs` | The review workflow. |

Tests: `tests/conformance.rs` (markers, digests, records, policy), `tests/conformance_state.rs` (required facts, completeness, conditions, obligation conformance), `tests/conformance_work.rs` (the work list, packets, attest, reviews, gates, documents), `tests/conformance_mcp.rs` (roles, stamping, the review server, the review workflow), and `tests/conformance_cli.rs` (`conseqa conform`, its exit codes and gates).
