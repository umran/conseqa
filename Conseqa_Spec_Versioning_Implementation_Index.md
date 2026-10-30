# Conseqa Spec Versioning Implementation Index

**Read this first if you are an agent asked to implement the spec-versioning specifications.** It is your brief:
- what to read;
- the order to build in;
- the rules of this repository;
- what is deliberately out of scope;
- how to report progress.

**Baseline:** `master` at `f301508`, where Parts I, IV and VI and the orchestrator design are all merged as Proposed documents and nothing of them is implemented.

---

## 1. What you are implementing

Three Proposed revision specifications, which together let Conseqa version a spec and guide its implementation:

| Part | Document | Adds | Depends on |
|---|---|---|---|
| **I** | `Conseqa_Spec_Change_Analysis_Revision_Specification.md` | Semantic diff of two spec trees, position-free obligation keys, the basis of each proof, durable facts, the obligation diff, the explanation invariant, the reliance delta, `conseqa diff`, the `diff_spec` MCP tool, obligation report format 10. | the current analyzer and design layer |
| **IV** | `Conseqa_Conformance_and_Implementation_Guidance_Revision_Specification.md` | Binding markers in any text file, per-declaration conformance records, completeness claims, conformance state, "proven" versus "conformant", the derived work list, work items with load-bearing checks, attest, reviews, `conseqa conform`, five MCP tools. | Part I |
| **VI** | `Conseqa_Realization_Profiles_Revision_Specification.md` | A capability vocabulary derived from the spec, a catalogue of technologies, a per-project stack profile, realization choice and feasibility, realization requirements as facts, deterministic checkers, `conseqa realize`, three MCP tools. It also amends Parts I and IV (Part VI §17, §18); those amendments are already applied to their texts. | Parts I and IV |

Also read, but do **not** implement:

- `Conseqa_Delivery_Loop_Orchestrator_Design.md` is an exploration of an external orchestrator that would drive this tooling. Its §12 lists tooling properties an orchestrator needs. Seven of them (T1–T7) are gaps in Parts I and IV that are pending a decision (§6 of this index).
- Parts II, III and V are outlined in Part I §21 only. They are not specified, and nothing of them is built here.

None of the three parts changes the DSL contract or any verdict.

---

## 2. Read first, in this order

1. `docs/live/README.md`, for the precedence rule. The semantics contract (`docs/live/CONSEQA_DSL_SEMANTICS.md`) is normative for DSL meaning, then the code and tests, then the other live docs. Root-level revision specs are historical records, except the three you are implementing: they are the design.
2. `docs/live/CONSEQA_DSL_SEMANTICS.md`. Read §1, §2, §10, §17, §20 and §25 closely, and skim the rest. Every capability, fact and condition in the specs uses this document's meanings.
3. `docs/live/PROJECT_STATE.md`, for how `.conseqa/` is laid out, loaded, locked and scanned. Parts IV and VI add directories beside the spec without touching the store format.
4. Part I in full, then Part IV, then Part VI.
5. The orchestrator design, §12 only.
6. The code you will build on:

| Area | Files |
|---|---|
| Obligation report and verdicts | `src/analyzer/report.rs`, `src/analyzer/verification/mod.rs` and its modules |
| Symbol graph, fingerprints and invalidation | `src/design/symbol.rs`, `graph_build.rs`, `graph_query.rs`, `fingerprint.rs`, `read_set.rs`, `invalidation.rs` |
| Project store | `src/design/store.rs` (`load_workspace`, `scan`) |
| Project CLI | `src/main.rs` |
| Design MCP server, with its routers and roles | `src/design/mcp.rs` |
| Agent backends, for Part IV's reviews | `src/harness/backend.rs`, `src/harness/backends/` |

---

## 3. House rules

These are the maintainer's standing rules for this repository. They are not negotiable within a milestone.

1. **Two binaries only:** `conseqa` and `conseqa-design-mcp`. Every new feature is a `conseqa` subcommand (`diff`, `conform`, `realize`), a flag on an existing one, or a design MCP tool. Never add a `[[bin]]`, an example target, or a stub. Dev tooling lives in tests.
2. **The design layer is called `design`.** Never introduce another name, alias or shim for it, and never refer to a former name in code, comments or docs.
3. **No umbrella "consistency".** Never use it as the umbrella term for serializability and ordering. Say "serializability and ordering", "commit-order evidence", "transaction proofs". The word is fine in its established meanings: `replay_consistent`, consistent hashing.
4. **Retired vocabulary never returns:** `invocation_lock`, `execution_handoff`, `validate_version`, `bump_version`, and transaction step `write`.
5. **Never run `cargo fmt` over the whole tree.** It is not clean at `HEAD` and churns unrelated files. Format only the files you touched (`rustfmt <file>`), or revert formatting-only changes before committing.
6. **Version counters move only as the specs say:**

   | Counter | Value |
   |---|---|
   | DSL contract | 6, unchanged |
   | Store `FORMAT` | 10, unchanged |
   | Obligation report `FORMAT` | 9 → 10, in M1 |
   | `FACT_FORMAT` | 1, including the `realization` kind |
   | Diff document format | 1 |
   | Conformance format | 1 |
   | `REALIZATION_FORMAT` | 1 |

   Breaks are clean, as in every earlier revision: no migration shims, no aliases.
7. **Match the surrounding code:** its naming, its idiom, and its comment density. Module and type doc comments here explain *why*, in prose, citing the spec section they implement. Follow that.
8. **Git discipline:**
   - Before every commit, check `git rev-parse --abbrev-ref HEAD` and the PR's state. A merged PR's branch is finished, and work never lands on `master` directly.
   - One branch and one PR per milestone.
   - Never merge your own PR, and never force-push.
9. **Green before every PR:** `cargo test` and `cargo clippy --all-targets`. Re-bless a fixture report only when the change intends the new output: `CONSEQA_BLESS=1 cargo test --test report fixture_report_matches_the_checker`. Say in the PR why the output changed.
10. **Soundness first.** None of these parts may change a verdict. A changed verdict in any fixture is a bug in your change, never a test to update.
11. **Specs and code never disagree.** The three specs are Proposed and may be wrong, or silent, somewhere. When implementation shows that:
    1. stop and ask the maintainer;
    2. with approval, amend the spec in the same PR as the code;
    3. record the decision in the PR description.

    Never resolve a spec question silently in code.
12. **Docs follow the code.** When a part is complete, carry out its "Documentation reconciliation" section (Part I §22, Part IV §22, Part VI §23). That means a new `docs/live/` document, the live README's tables, `PROJECT_STATE.md`, and listing the spec as historical.
13. **System One.** None of these milestones touches the System One layer. If one turns out to, stop and ask first.

---

## 4. Build order

Each milestone is one branch and one PR. It is done when its tests from the spec's test plan pass, its acceptance criteria hold, and the house rules are met. Spec sections are given as `I §n` (Part I), `IV §n`, and `VI §n`.

```text
M1 ─ M2 ─ M3 ─┬─ M4 ─ M5 ─ M6 ─ M7 ─┬─ M9
              └──────── M8 ──────────┘
```

M8 needs only M1 and M2, so a second agent can run it beside M4–M7. M9 needs everything else.

### M1: Durable facts, obligation keys, and the basis

- **Spec:** I §9, §10 (including §10.9 and the `realization` kind's fields from VI §12), §17, §18.2–§18.4 and §18.8; VI §12.
- **Builds:**
  - a new `src/analyzer/basis.rs`:
    - `ObligationKey` and `Fact`;
    - the durable form, with `FACT_FORMAT`, `FACT_READS` and the two-phase probe;
    - the textual declaration key and its parser;
    - exhaustive extraction of basis and blocked-on sets from the structured proofs;
  - in `report.rs`: obligation `key`, `basis` and `blocked_on`, `ProverReport.fact_format`, and `FORMAT` 10;
  - absence facts exposed where proofs use them implicitly. `no_deleters` is the known case (I §18.4);
  - the optional report fields in `viz/src/types/report.ts`.
- **Tests:** I §19.3, §19.13 and §19.14, with golden files under `tests/golden/facts/`. Every proven obligation of every fixture has a non-empty basis.
- **Watch:** extraction must match every proof, obstacle and gap variant **without a wildcard arm** (I §10.8). That is what keeps a future proof route from silently producing an empty basis.

### M2: Reading two trees, and the declaration partition

- **Spec:** I §6, §7, §8, §18.1, and §18.5 (`partition.rs`, `delta.rs`).
- **Builds:**
  - `load_workspace` factored over a file source: the directory, or a git tree read with `rev-parse`, `ls-tree` and `cat-file`;
  - empty-side reading;
  - version refusals naming the side;
  - the partition and its residual projections, and the authoring classification;
  - structural deltas.
- **Tests:** I §19.1 (the partition property test), I §19.6, I §19.8, and the source and refusal cases of I §19.10.

### M3: The obligation diff and `conseqa diff`

- **Spec:** I §11–§16, §18.5 (`diff.rs`), §18.6, §18.7, and §22.
- **Builds:**
  - matching by key, classes, restatement and scope flags;
  - explanation and impact, the reliance delta, and the diff document;
  - `conseqa diff` with `--merge-base`, `--report` and `--fail-on`;
  - the `diff_spec` shared MCP tool.
- **Also builds:** the mutation catalogue of I §19.2, as **library code** rather than test-only code. Part IV's load-bearing check (IV §12.2) applies the same withdrawals at runtime.
- **Tests:** I §19.2 (the explanation corpus), §19.4, §19.5, §19.7, §19.9, §19.11 and §19.12.
- **Gate:** zero unexplained entries across the explanation corpus (I §12.2). An unexplained entry means a missing anchor in M1's extraction. Fix the extraction; never the test.
- **Docs:** Part I's reconciliation (I §22).

### M4: Markers, regions and records

- **Spec:** IV §6, §8, §14.4, and §17.1 (`markers.rs`, `record.rs`, `policy.rs`).
- **Builds:**
  - the scan, over tracked plus untracked-but-not-ignored files, never `.conseqa/`;
  - marker parsing and diagnostics;
  - region residuals with nested regions elided, and their digests;
  - the strict record reader;
  - atomic, optimistic record writes;
  - version-axis degradation to `unattested`;
  - the policy file and its defaults.
- **Tests:** IV §18.1–§18.3.

### M5: Required facts, completeness, and conformance state

- **Spec:** IV §7, §9, §10, and §17.2 (conformance classes and implementers in `basis.rs`).
- **Builds:**
  - required facts from both sources: proofs, and (once M9 lands) realizations;
  - completeness claims re-checked on every derivation;
  - every condition, and obligation conformance;
  - the `conform status` derivation.
- **Tests:** IV §18.4 and §18.6.

### M6: The work list, work items, and attest

- **Spec:** IV §11, §12, §13, §14.1 and §14.2.
- **Builds:**
  - priorities, dependency order and scoping;
  - packets with the load-bearing check (using M3's mutation library), precedents and guide topics;
  - read sets and stale refusal;
  - attest validation and writing.
- **Tests:** IV §18.5, §18.7 and §18.8.

### M7: Reviews and the conformance surfaces

- **Spec:** IV §14.3, §14.5, §15, §17.3–§17.6, and §22.
- **Builds:**
  - the review workflow through the existing harness backends, with fresh context and scratch configuration outside the repository;
  - `conseqa conform` (`status`, `plan`, `item`, `attest`, `review`, `scan`, `check`);
  - the five MCP tools and their routers.
- **Tests:** IV §18.9–§18.11.
- **Docs:** Part IV's reconciliation (IV §22).

### M8: Capabilities, the catalogue, the stack, and choice

- **Spec:** VI §6–§10, §14, §15, and §19 (`capability.rs`, `catalogue.rs`, `stack.rs`, `choice.rs`).
- **Builds:**
  - the capability vocabulary and needs derivation;
  - the entry format and strict reader;
  - the standard catalogue embedded at build time from `catalogue/`, and the project catalogue;
  - the stack profile, candidates, validity, choice and infeasibility reports;
  - `conseqa realize` and `check --require-realizable`;
  - the three MCP tools.
- **Catalogue content:** start with the entries whose claims you can cite from vendor documentation: `postgres.database`, `kafka.keyed-topic`, `sqs.fifo`, `sqs.standard`, `envoy.ring-hash`. The entries in VI §7 are illustrative. Every capability, exclusion and requirement must carry a citation (VI §8.3). **Do not ship `kafka.consumer-group`** until VI §21's question is answered.
- **Tests:** VI §20.1–§20.3 and §20.7.

### M9: Realization requirements, checkers, and integration

- **Spec:** VI §11, §13, §17, §18 and §23; I §13.4 and §19.15; IV §18.12.
- **Builds:**
  - neighbour relations over the design graph's queries;
  - realization facts as Part IV required facts;
  - the record `realization` field and staleness;
  - stack-driven binding, the packet's Realization section, and the `unrealizable` condition;
  - the five checker kinds and checker reviews;
  - Part I's realization section and its `infeasible` and `ambiguous` gates.
- **Tests:** VI §20.4–§20.6 and §20.8, I §19.15, and IV §18.12.
- **Docs:** Part VI's reconciliation (VI §23).

---

## 5. Out of scope

Do not build, stub, or leave hooks for:

- Parts II, III and V;
- the orchestrator in `Conseqa_Delivery_Loop_Orchestrator_Design.md`, which lives in its own repository if it is ever built, or its spikes;
- caches, or any non-durable storage concept (VI §21);
- per-environment stacks, third-party catalogue registries, or anomaly-level isolation detail (VI §21);
- any change to the DSL contract, to verdicts, or to the store format.

---

## 6. Decisions pending

Ask the maintainer before you reach the milestone named. Do not decide these yourself.

T1–T7 come from the orchestrator design's §12. Each is a small amendment to Part IV, or to Part I. The recommendation for all seven is to accept, amending the spec in the PR that implements the affected section.

| Decision | Needed by |
|---|---|
| T1: documented, versioned JSON output for every `conform` and `realize` command | M7, and M8 for `realize` |
| T2: `--spec-range` scoping for `conform plan` and `conform check` | M6 |
| T3: plan items carry `depends_on` and `files` | M6 |
| T4: implementer and reviewer roles on the design MCP server | M7 |
| T5: `by` stamped from the credential, not supplied by the agent | M6 |
| T6: replaying an identical attest or review is a no-op success | M6 |
| T7: packets carry open disputes and the last refusal reason | M6 |
| The `kafka.consumer-group` entry's reading of `consistent_hash` (VI §21). Recommendation: confirm it against §10.6 of the semantics contract, or ship the entry without the `assignment` claim. | M8 |
| Anything a spec leaves undecided. Ask; never pick silently (house rule 11). | when found |

---

## 7. Progress

Update this table in every milestone PR.

| Milestone | State | Branch | PR |
|---|---|---|---|
| M1 Durable facts, keys, basis | in review | `spec-versioning-m1` | #43 |
| M2 Two trees, partition | in review | `spec-versioning-m2` | #44 (stacked on #43) |
| M3 Obligation diff, `conseqa diff` | in review | `spec-versioning-m3` | #45 (stacked on #44) |
| M4 Markers, regions, records | in review | `spec-versioning-m4` | #46 (stacked on #45) |
| M5 Required facts, state | not started | | |
| M6 Work list, items, attest | not started | | |
| M7 Reviews, surfaces | not started | | |
| M8 Capabilities, catalogue, choice | not started | | |
| M9 Realization integration | not started | | |

---

## 8. How to work a milestone, and what to report

1. **Plan.** Re-read the milestone's spec sections and the code they touch. Write a short plan in the PR description: scope, files, risks, and spec questions.
2. **Ask.** Settle any spec question or pending decision with the maintainer before writing the code it affects.
3. **Implement with tests.** Implement in the order the spec's test plan suggests, writing each test with the code it covers.
4. **Check.** Run `cargo test` and `cargo clippy --all-targets`. `rustfmt` only the files you touched. Grep your diff for the vocabulary house rules 2–4 forbid.
5. **Open the PR.** Its description covers:
   - what was built, by spec section;
   - tests added;
   - any spec amendment, with the maintainer's approval quoted;
   - re-blessed fixtures and why;
   - what is deferred and why.

   Update §7's progress table in the same PR.
6. **Stop.** Do not start the next milestone until the PR is reviewed, unless the maintainer says to continue.

## 9. Start here

Begin with **M1**. Your first message is a plan, before any code. It covers:
- your reading of M1's scope;
- the files you expect to create or change;
- how you will make basis extraction exhaustive;
- the spec questions you have.

Then implement it on a branch named `spec-versioning-m1`.
