# Spec Change Analysis

Conseqa treats a change to a spec as an object of its own. Given two spec trees, typically two git commits of a project's `.conseqa/`, it reports:

1. what changed in the architecture: the declarations added, removed, or modified, each change attributed to exactly one declaration;
2. what changed in what the architecture proves: every obligation added, removed, regressed, improved, held, or held on a different argument;
3. why: for every changed verdict, the changed declarations that explain it;
4. what the implementation must now provide: the facts newly relied on, and no longer relied on, by any proof.

This document describes the implementation. The design record is `Conseqa_Spec_Change_Analysis_Revision_Specification.md` (Part I), now historical. Where they differ, the code and its tests win (see the README's precedence rule). The DSL's meaning is the semantics contract's. Nothing here changes a verdict: change analysis reads verdicts, it never makes them.

**Not yet implemented.** The realization changes of Part I §13.4 (stack profiles, needs, chosen realizations, and `realization` facts), the document's `realization` section, and the `infeasible` and `ambiguous` gates arrive with the realization profiles revision (Part VI).

## Surfaces

| Surface | What it does |
|---|---|
| `conseqa diff [<base>] [<head>]` | `<base>` is a commit expression (default `HEAD`); `<head>` is a commit expression or, by default, the working tree. `--merge-base` reads the base at the merge base of `<base>` and the head, the working tree counting as `HEAD`. `--report <path>` writes the diff document. `--fail-on <class>[,<class>…]` names the gates. Prints the human output to stdout. |
| Design MCP `diff_spec` | `base` (default `HEAD`) and `merge_base` (default `false`). Served to every role, as `requirement_report` is. The head is the engine's head, which adopts the project's files before every call. Answers with two text blocks: a summary, then the diff document as compact JSON. Read-only. A base that cannot be read is an in-band error. |
| `conseqa check --report` | The obligation report, format 10: every obligation's `key`, a proven one's `basis`, and an unproven one's `blocked_on`, in the fact vocabulary its `fact_format` names. A single report already answers "which proofs rest on this declaration". |

`conseqa diff` exits:

| Exit | Meaning |
|---|---|
| 0 | The diff was produced, and no named gate failed. |
| 1 | The diff could not be produced. Either a side did not load, was of another store format or DSL version, or named a ref that resolves to no commit, or a git side was named for a project outside any git repository. The message names the side: `base (origin/master): …`. |
| 2 | A named gate failed. A gate named while the obligation diff is unavailable cannot be evaluated, and fails. |

| Gate | Fails when |
|---|---|
| `regressed` | any obligation regressed |
| `removed` | any obligation was removed, whatever its base status |
| `added-unproven` | any added obligation is not proven |
| `rebased` | any obligation was rebased |
| `unexplained` | any entry is unexplained |

`conseqa diff` does not replace `conseqa check`. CI runs both: `check` judges the head alone, and `diff` judges the change, for example `conseqa diff origin/master HEAD --merge-base --fail-on regressed,removed`.

## Reading two trees

`src/design/change/side.rs`, over `src/design/store.rs`.

Both sides are loaded by the store's own loader, `load_source`, over a `FileSource`:

- `ProjectDirectory` reads the project's `.conseqa/` without the writer lock;
- `FileTree` holds files in memory, and `read_commit` fills one from a commit with `git ls-tree -r -z` and `git cat-file --batch`.

Every refusal therefore applies to both sources alike: another store format or DSL version (naming the side and both versions), an unexpected entry, a file name that is not its id's canonical encoding, a file that does not load. A committed symbolic link or submodule under `spec/` is refused by name. This is the one place a commit and the working tree differ, since the working tree follows the link.

Reading a side checks nothing out, makes no worktree, touches no index, writes no file, and takes no lock. Git runs with `GIT_OPTIONAL_LOCKS=0`. The project is found from the working directory, and its path relative to the repository root is the path read at every commit. A commit holding no project there is the empty project, so a branch that introduces a project adds everything. A ref that names no commit is an error, never an empty side. Both sides are analyzed by the running build, so a diff measures a change in the model, never a change in the analyzer.

## The declaration partition

`src/design/change/partition.rs`.

Every semantic fact of a workspace belongs to exactly one declaration, so a change is reported once, at that declaration. The members, in the order a diff lists them (L0, then L1, then intent):

`service`, `schema`, `data_model`, `data_object`, `outbox`, `topic`, `state_machine`, `transition`, `operation`, `input`, `program`, `transaction`, `effect_site`, `requirement`, `topic_runtime`, `subscription_runtime`, `outbox_runtime`, `execution_pool`, `router`, `storage_layout`, `prompt_obligation`.

A member's **residual** is its canonical form minus what the members it contains own:

- an operation's residual is its service;
- a program holds each inline transaction, and each effect contract, by id;
- a transaction drops its id and requirements, and holds the effect contracts of its intent and outbox-write steps by id;
- a data model drops its objects and outboxes, and a state machine its transitions;
- a schema drops its description;
- a requirement owns everything but its key, `result` included.

The projections destructure every workspace struct with every field named and match every step kind without a wildcard, so a new field or step fails to compile until the partition assigns it.

**Authoring content** is classified apart, reported, and never semantic: an operation's sketch, stage, and description; a schema's description; `intent/proposals.yaml`; and the project's settings (`project.yaml` but its `format` and `dsl`).

**The invariant.** Changing any one leaf of a workspace changes exactly one residual or one authoring item. A leaf that spells an identity is the exception: an inline transaction's id, an effect id, or a requirement's key renames exactly the members it names, plus at most the one member naming them. A property test perturbs every leaf of every fixture to check it.

## Declaration changes and deltas

`src/design/change/diff.rs`, `src/design/change/delta.rs`.

A declaration only on head is `added`, only on base `removed`, and on both with different residuals `modified`. Ids are identity: nothing pairs a removal with an addition, so a rename is a removal and an addition. A modified declaration carries a structural delta. Maps align by key, lists of elements with unique ids align by id, and other lists align by their longest common subsequence, so an inserted step is one insertion. Unaligned elements pair by position: the same kind recurses, and a different kind is one replacement. A moved element is written `steps[2→3]`. The delta is presentation; nothing classifies by it.

## Obligation keys

`src/analyzer/basis/mod.rs`.

An obligation keeps its positional `id` as its address within one report. Its `key` is its identity across reports. The key has four parts:

- its subject: the operation, or the operation and transaction;
- its property: one of the five a requirement produces;
- its requirement key: the value references identifying the requirement;
- its occurrence: which of the requirements sharing the first three it is.

Reordering requirements changes no key. Changing a requirement's key is a different obligation. Changing its **terms** restates the same one: an ordering requirement's `position`, a recoverability requirement's `completion`. A result replay obligation shares its idempotency requirement's key, with property `result_replay`.

## The basis and the blocked-on set

`src/analyzer/basis/` (`extract.rs` for extraction, `durable.rs` for the durable form, `declaration.rs` for declaration keys).

A proven obligation's **basis** is the set of facts its proof consumed. A fact is either declared (an isolation level, a lock, a guarded mutation, a cursor, a fence, a delivery, a message identity, a commit-deduplication key, an external guarantee) or a model-wide query result (a conflict closure, its cyclic components, the absence of any deleter of an object, a consumer's keyed requirement). Facts never name a step or a position. Each is anchored to every declaration whose content its truth was read from. Delivery is the one L1 fact. A proof that rests on another obligation cites `relies_on(key)`, anchored to that obligation's requirement, and never inlines its basis.

An unproven obligation has a **blocked-on set** instead:

- `present`: the facts its obstacles found present but insufficient;
- `missing`: the facts they found missing, stated only where a gap names the premise its route lacks exactly. These are a lock over the access's selector, an at-least-once delivery, `no_deleters`, a naturally replayable body, and a requirement keyed from a consumer's or target's input (`keyed_requirement`);
- `anchors`: every declaration the obstacles read.

Extraction matches every proof, evidence, obstacle, and gap variant without a wildcard, so a new proof route fails to compile until it says what it rests on.

**Durable form.** A fact is a map tagged by `kind`, in canonical DSL form with sets as sorted lists. Its identity is its canonical text: compact JSON with every map's keys sorted. A reader re-serializes what it parsed and refuses any difference by path, so one fact has exactly one spelling. `FACT_FORMAT` (1) counts the vocabulary, and `FACT_READS` is `[1]`. A record carrying facts records both its `dsl` and its `fact_format`. Every kind has a golden file under `tests/golden/facts/`. A change to a golden sample without a `FACT_FORMAT` bump fails, and re-blessing refuses to rewrite it.

**Declaration keys** have a structured form, the `kind` / `value` map, and a textual form for hand-written references: `transaction operation.apply_payment/tx.apply_payment`, with each id escaped as the store escapes a file name. A requirement has no textual form.

## The obligation diff

`src/design/change/diff.rs`.

**Availability.** Each side must assemble into a model and validate, as `conseqa check` requires. If one does not, the obligation diff is unavailable, naming the side, the reason, and the other side's counts when it analyzed. The base is named when both fail. The declaration diff is still produced.

**Classes.** Every obligation of either side is in exactly one class. Statuses order `proven` > `unknown` > `disproven`.

| Class | When |
|---|---|
| `added` | its key is on head only |
| `removed` | its key is on base only |
| `regressed` | matched; head's status is below base's |
| `improved` | matched; head's status is above base's |
| `held` | matched; proven on both on the same basis (facts compared by canonical form, anchors ignored) |
| `rebased` | matched; proven on both on different bases. It carries `withdrawn` and `introduced` facts. |
| `unproven` | matched; unknown on both or disproven on both. It carries `blocked_on_changed`. |

Two flags apply to matched obligations:

- `restated`: the requirement's terms differ.
- `scope`: a proof moved between `l0_only` and `runtime_dependent`. `through` names the relied-on obligations whose own scope moved; a `held` obligation's scope moves only that way.

**Explanation.** A matched entry's `explained_by` lists the changed declarations among the anchors of either side's basis or blocked-on set, and among the members of their query facts. A conflict closure or its components record their members on their side. The absence queries, `no_deleters` and `keyed_requirement`, are read on both sides, since what falsifies one on one side is what the other side declares: a deleting transaction, or a requirement keyed from the input. An added or removed obligation is explained by its requirement, when that was added, removed, or modified, and by any subject declaration added or removed with it. `explained_through` names the relied-on obligations whose own entries changed.

**The invariant.** Every `regressed`, `improved`, or `rebased` entry, and every `unproven` entry whose blocked-on set changed, must be explained: by a changed declaration, or through a relied-on obligation whose change is itself explained. The rule is well-founded, so obligations that changed only through each other are not explained. One build analyzes both sides, so an unexplained entry means the extraction failed to anchor something the verifier read. It is listed under `unexplained` with both sides' grounds, never dropped. The explanation corpus (below) holds none.

**Listing.** `counts` gives every class, quiet obligations included. `entries` lists, in key order, every obligation that changed or that a changed declaration touches. A `held` entry, or an `unproven` one with the same blocked-on set, is quiet and left out when it carries no flag and no changed declaration touches it. `removed` groups removed obligations by the outermost removed operation, transaction, or requirement, or else by the requirement whose change dropped them. Removing a requirement is allowed, but never silent, and the `removed` gate catches it.

**Impact.** For every changed declaration, `impact` lists the obligations of either side whose basis or blocked-on set anchors it, each with its class: the view a reviewer of one changed file wants.

**Reliance.** A side's reliance is the union of its proven obligations' bases. `reliance.introduced` lists the facts some proven obligation on head relies on and none on base, with `relied_on_by`. `reliance.withdrawn` lists the reverse. A withdrawn fact is one no proof needs any longer, never one the implementation may stop providing.

## The diff document

`DiffDocument` in `src/design/change/diff.rs`. Format 1, with `dsl` and `fact_format`. Every struct denies unknown fields. The same two trees give a byte-identical document on any machine.

```json
{
  "format": 1, "dsl": 6, "fact_format": 1,
  "base": { "source": { "commit": "…", "ref": "origin/master", "merge_base": true }, "empty": false },
  "head": { "source": { "working_tree": true }, "empty": false },
  "declarations": { "changes": [ … ], "authoring": [ … ] },
  "obligations": {
    "status": "available",
    "counts": { "added": 0, "removed": 0, "regressed": 1, "improved": 0, "held": 5, "rebased": 0, "unproven": 5 },
    "entries": [ { "key": { … }, "base_id": "…", "head_id": "…", "class": "regressed",
                   "base_status": "proven", "head_status": "unknown", "restated": false,
                   "explained_by": [ { "kind": "subscription_runtime", "value": { … } } ] } ],
    "removed": [],
    "unexplained": []
  },
  "impact": [ { "declaration": { … }, "obligations": [ { "key": { … }, "class": "regressed" } ] } ],
  "reliance": { "introduced": [], "withdrawn": [ { "fact": { "kind": "delivery", … }, "layer": "l1" } ] }
}
```

When the obligation diff is unavailable, `obligations` is `{ "status": "unavailable", "side": …, "reason": …, "other_side": { "proven": …, "unknown": …, "disproven": … } }`, and `impact` and `reliance` are absent.

The human output (`src/design/change/render.rs`) prints the same content: the declaration changes with their deltas, the entries by class with what explains them, then the removed and unexplained sections and the reliance. It never says what the document does not.

## The mutation catalogue

`src/design/change/mutation.rs`.

The catalogue is library code, so the explanation corpus and Part IV's load-bearing check read one definition of what withdrawing a fact means. `catalogue(workspace)` applies each mutation at every site it fits:

- remove a lock: every lock step of the transaction with that selector and mode;
- drop isolation to `unspecified`, or weaken it one level;
- turn a `compare_and_set` into an `update`, dropping a rejection block the transaction no longer needs;
- add a transaction deleting a versioned object right after one that selects it by values still in scope;
- change a subscription's delivery, or an external effect's idempotency, to each other value;
- remove a `deduplicated_by`;
- widen a read, update, delete, or lock selector to `all`;
- remove a requirement;
- unspecify an external effect's result replay, a request's retry, or a channel's message identity.

`withdrawal(fact)` maps a fact to the mutation withdrawing it, and `withdraw(workspace, fact)` applies it. Facts without one fall back to listing the obligations relying on them. These are a guarded mutation by a transition, cursor advance, fence, or upsert (no comparison to drop), and every query fact. `Mutation::falsifies(fact)` says which facts a mutation makes false.

**The explanation corpus** (`tests/obligation_diff.rs`) diffs every fixture against every mutant. A mutant that does not validate is skipped, but every mutation kind must verify somewhere. For each mutant:

- at least one declaration changes;
- every obligation whose basis held a falsified fact changes class;
- a removed requirement's obligations are removed;
- no entry is unexplained.

## Tests

| Test | Covers |
|---|---|
| `tests/basis.rs` | Every proven obligation has a non-empty basis and every unproven one a non-empty blocked-on set; keys round-trip; reordering keeps keys; extracted facts round-trip; golden samples; non-canonical spellings and other versions refused by name; textual keys round-trip. |
| `src/design/change/partition.rs` (unit) | The partition invariant over every leaf of every fixture; authoring only; a renamed transaction. |
| `src/design/change/delta.rs` (unit) | Each alignment rule. |
| `tests/obligation_diff.rs` | The explanation corpus; reordering; rebasing; a runtime change; authoring only; an added deleter; a renamed transaction; restatement and result replay; determinism; unavailable and empty sides; each gate. |
| `tests/spec_change.rs` | Sides read from real git repositories, refusals naming the side, no writes and no lock; `conseqa diff`'s output, report, determinism across runs, `--merge-base`, and exit codes. |
| `tests/design_mcp.rs` | `diff_spec` for both roles, adopting edits to the files, the merge base, and an unreadable base. |
| `tests/report.rs` | The fixture report at format 10. |
