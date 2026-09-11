# Conseqa External Boundary Guarantees and Decision Vocabulary Revision Specification

## 1. Status and scope

**Version 5.** Supersedes v4, adding **Part V — MCP server version awareness**: the server that agents learn the DSL from states, at the initialize handshake and in every served brief, which contract version it speaks. Part V is what makes Part IV's "fanout workers are version-synchronized by construction" true rather than asserted — the same daemon compiles one `DSL_VERSION`, and its handshake, instructions, and guide all cite it.

v4 added **Part IV — DSL contract versioning**: the DSL itself acquires a declared version, every exported specification states which version it is authored in, and version mismatch is refused by name. Part IV is the document-level completion of the clean-break policy v3 adopted at the store level: a break is never surfaced as a serde accident — it is named.

This remains a **clean semantic revision**: no backwards compatibility, no deprecated external-effect representation, no migration or canonicalization semantics. Superseded syntax fails schema validation; models using it are rewritten against the revised DSL. This follows established project practice — the stored-workspace format was already broken once, by version refusal rather than migration (`FORMAT: u64 = 2`, persistence.rs:47).

This revision closes the two expressiveness gaps surfaced by the `pixieset-clone` model (revision 81), whose requirement report proves 52 of 54 obligations and returns `unknown` for exactly two:

- `oblig.operation.notify_gallery_published.idempotency.0` — the client-email presence check is inexpressible, forcing a `condition: unspecified` branch that can never satisfy decision replay;
- `oblig.operation.request_photo_upload.idempotency.0` — the presign boundary's true guarantee (harmless to repeat, result fresh per attempt) has no declarable form, and the `match_result` over its per-attempt result is an unremovable control-leg obstacle even though both arms are bare terminals.

Five parts, separately landable (Parts IV and V ride Part II's release):

- **Part I — `present`**: a presence predicate in the program `Condition` vocabulary, with a companion definition of `eq` over absent values.
- **Part II — external boundary guarantees**: replace the external effect's single mechanism fact (`IdempotencyGuarantee`) with three orthogonal declared properties — **interaction identity**, **duplicate-side-effect behaviour**, and **terminal-result replay stability**.
- **Part III — idempotency-inert continuation admissibility**: a narrow, structurally derived exception to the idempotency control leg for decisions whose every continuation to a terminal performs no additional modeled work.
- **Part IV — DSL contract versioning**: a single declared version naming the normative semantic contract; specifications, exports, reports, and the served guide all cite it; mismatch and absence are refused by name.
- **Part V — MCP server version awareness**: the server declares its DSL version at the initialize handshake and in its instructions; the version travels on the three independent axes (MCP protocol, server build, DSL contract) without conflation; the connection declares, the document boundary refuses.

The governing principles remain: `unspecified` is epistemic absence of a usable fact (§1.1); absence of a guarantee is not evidence of a violation (§1.2); the DSL declares semantic properties above the implementation-mechanism floor; declared guarantees, structural facts, and analyzer-derived proof facts remain distinguishable.

## 2. Goals

1. Make optional-field presence expressible without introducing a general expression language.
2. Separate external interaction identity from duplicate-work behaviour, and duplicate-work behaviour from terminal-result replay, so all four quadrants of (duplicate-safe?, result-stable?) are honestly declarable.
3. Represent harmless repeatable external boundaries with fresh results, and keyed externally idempotent boundaries with or without replay-stable results.
4. Align external-boundary analysis with the decomposition request boundaries already use — duplicate-safety and result-stability consulted independently, proven for requests, declared for externals (§1.3).
5. Admit non-replaying decisions exactly when divergence provably cannot change modeled work, recording the admission as **derived proof evidence**, never as an implementation assumption.
6. Keep the implementation minimal: one authoritative representation, plain serde derives, no compatibility machinery of any kind.
7. Name the semantic contract: every long-lived artifact — export, report, served guide — states the DSL version it speaks, and every break is refused by version, never surfaced as a parse accident.
8. Make the server self-describing: an agent connecting to it learns the DSL version from the handshake and the instructions before it authors anything, so what it learns and what the server enforces are the same contract.

## 3. Non-goals

- **No compatibility layer**: no legacy detection, compatibility parser, custom migration deserializer, automatic rewriting, deprecated aliases, or migration diagnostics.
- **No multi-version interpretation.** Part IV is declare-and-refuse, not a server that hosts or evaluates multiple DSL versions side by side. A build speaks exactly one version; anything else is refused by name. Multi-version hosting would reintroduce, at higher cost, the compatibility burden the clean break just removed.
- **No semver.** The DSL version is a single monotonically increasing integer, house style (`Revision`, `FORMAT`). Under a clean-break policy there is no "compatible minor" consumer to serve; a normative change bumps, anything else does not.
- **Row absence.** No predicate over a transaction read's cardinality (zero-match read). The `unspecified` branches guarding row absence in `grant_gallery_access` and `record_photo_download` remain, invisible to the report because those operations deliberately declare no requirements. Defers to the same open cluster as §26's insert-failure semantics.
- **No general expression language**, arbitrary boolean functions, or collection predicates. No selector-level `present` (only the `eq`-over-absent clarification reaches selectors).
- **No general history compatibility.** No work-equivalence reasoning beyond Part III's structural shape.
- **No generic identity abstraction** spanning requests, messages, and external interactions. External identity mirrors the *shape* of `RequestInput.identity` and `message_identity` but remains its own vocabulary, per the outbox/topic precedent.
- **Transactions untouched.** `Transaction.idempotency: IdempotencyGuarantee` is a modeled logical commit guarantee load-bearing for route-B artifact recovery. `IdempotencyGuarantee` becomes transaction-only. Request identity, topic/outbox message identity, and outbox/subscription/async/batching semantics are unchanged.

---

# Part I — the `present` condition

## 4. L0 — `Condition::Present`

```yaml
- kind: branch
  condition:
    kind: not
    condition:
      kind: present
      value:
        source: input:input.notify_gallery_published.outbox
        path: client_email
  then:
    steps:
    - kind: complete
```

```rust
pub enum Condition {
    Unspecified,
    Eq { value: ValueRef, equals: SelectorValue },
    And { conditions: Vec<Condition> },
    Not { condition: Box<Condition> },

    /// Holds iff the referenced path resolves to a value.
    Present { value: ValueRef },
}
```

Declared in `src/spec/operation/program.rs` alongside the existing variants (`Condition` at line 257).

## 5. Semantics

### 5.1 Presence

`present { value }` holds iff the referenced path resolves to a value in the attempt's evaluation context. A path is **absent** iff any optional segment required to resolve it is absent: `present a.b` with `a` absent does not hold. Presence is part of the **logical value** Conseqa observes; two attempts observing replay-equivalent roots agree on the presence or absence of any path within them.

`present` is therefore a **deterministic function of its single root**, joining the §16 sentence "`eq`, `and`, and `not` are deterministic functions of their references" verbatim. `Condition::roots()` returns the one reference; `Condition::is_deterministic()` returns true. The existing decision-replay rule applies with no `present`-specific theorem: deterministic condition + replay-stable roots ⇒ the decision replays. §18 needs no amendment — rule 3 already carries optionality as part of payload equality, which is the entire mechanism by which `notify_gallery_published`'s obligation discharges.

There is no `absent` kind; `not { present … }` composes.

### 5.2 Absence is not a value

Previously silent, now normative: **`eq` does not hold when either operand evaluates absent** — including both absent. Absence is not a distinguished comparable value; presence is queried only through `present`. Consequently `not { eq A B }` holds when either operand is absent; an author needing different behaviour composes `present` with `eq` explicitly. The same rule applies to `SelectorPredicate::Eq` over a stored optional field: the conjunct does not match that instance.

### 5.3 Presence over required paths is redundant, not invalid

`present` over a statically required path is semantically well-defined — vacuously true — and **SHALL NOT be rejected**. Validation emits a **`Severity::Warning`** diagnostic, `RedundantPresenceCheck`, when the complete resolved path contains no optional segment (infrastructure exists: `Severity::Warning` in `analyzer/diagnostic.rs:24`, model-wide `notes` in `report.rs:52`). The warning MUST NOT affect validity or proof semantics.

One consequence is stated normatively in the doc: **conditions never prune admitted paths** — V1 performs no constant folding, so the never-taken arm of a vacuous `present` remains an admitted path and is analyzed like any other. The redundancy warning exists precisely so authors delete dead arms instead of carrying phantom obligations through them.

## 6. Analyzer

- `Condition::roots` / `is_deterministic` gain the arm (program.rs:281,302); root resolution and scope use the existing §11/§16 reference machinery unchanged (a `present` over a `transaction_output` is legal exactly where the output is definitely available).
- Evidence rendering describes the fact explicitly: *"condition `present(input.client_email)` is deterministic and its root is replay-stable."*

---

# Part II — external boundary guarantees

## 7. Design

An external boundary exposes three independent semantic dimensions — interaction identity, duplicate-side-effect behaviour, and terminal-result replay behaviour — and they must not be conflated: same logical interaction ≠ safe duplicate application, and safe duplicate application ≠ same terminal result on replay. The retired `deduplicated_by` conflated all three (§13.3: work collapse *and* terminal-result fixing, keyed by one declaration), leaving the off-diagonal combinations inexpressible: side effects harmless to repeat with a fresh result every time (presign); collapsed work with a generic-ack response; a pure computation with per-call ancillary work.

## 8. L0 — `ExternalEffect`

```rust
pub struct ExternalEffect {
    pub name: String,

    /// What identifies one logical external interaction.
    pub identity: ExternalIdentity,

    /// Duplicate-side-effect behaviour, relative to that identity.
    pub idempotency: ExternalIdempotency,

    /// Terminal-result replay behaviour, relative to that identity.
    pub result_replay: ExternalResultReplay,

    pub result: Option<ResultType>,
}

#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExternalIdentity {
    Unspecified,
    Keyed { key: ExternalIdentityKey },
}

pub struct ExternalIdentityKey {
    pub components: Vec<ValueRef>,
}

#[serde(rename_all = "snake_case")]
pub enum ExternalIdempotency {
    Unspecified,
    Distinguishable,
    IdenticalPerIdentity,
    SideEffectFree,
}

#[serde(rename_all = "snake_case")]
pub enum ExternalResultReplay {
    Unspecified,
    Unstable,
    ReplayStable,
}
```

Declared in `src/spec/operation/effect.rs`, replacing `ExternalEffect.idempotency: IdempotencyGuarantee` (effect.rs:88). **Plain serde derives throughout** — with no legacy surface to detect, no custom `Deserialize` exists anywhere in this revision.

`ExternalIdentityKey` is deliberately distinct from the public `IdempotencyKey` type: an interaction identity is not itself an idempotency declaration. Internal verifier utilities (root collection, stability judgment) are shared; the public semantic concepts remain distinct. Key components are value references evaluated at the effect's execution or establishment site under the existing §13 rules, so intents and transition side-effects need no special handling.

## 9. Semantics

### 9.1 `identity`

`keyed { key }`: **equal evaluated key tuples identify applications of one logical external interaction.** Identity alone establishes nothing about what repeated applications do — it implies neither deduplication, nor idempotency, nor result replay, nor at-most-once application. `unspecified`: Conseqa has no usable sameness relation across applications of that boundary. A keyed identity with both behavioural fields `unspecified` is valid: it declares identity while withholding behaviour.

### 9.2 `idempotency` — duplicate-side-effect behaviour

An **implementation guarantee** (§1), conditional per §1.3, quantified over applications:

- `unspecified` — no usable fact (§1.1). Epistemic: not "safe", not "unsafe".
- `distinguishable` — an explicit negative property, stronger than `unspecified`: repeated applications may produce distinguishable modeled externally observable work (an unkeyed payment charge). Requires no identity. The analyzer treats it as a direct obstacle wherever duplicate execution is admitted.
- `identical_per_identity` — requires `identity: keyed`. Across applications of one interaction, any number of applications produces modeled externally observable side-effect work indistinguishable from exactly one application, **under every admitted interleaving**. The interleaving qualification is normative: `set X=5; other interaction sets X=8; duplicate sets X=5` may be distinguishable from one application, so naive re-imposition does not qualify merely because the payload repeats. The author owns the conformance claim; the DSL declares the property, not the mechanism — provider-side deduplication, content-addressed processing, conditional mutation, and intrinsically idempotent protocol semantics are all valid realizations, and none is asserted by the declaration.
- `side_effect_free` — application causes no **modeled externally observable** state change beyond producing its synchronous result. Deliberately scoped to Conseqa-observable semantics: it does not prohibit unmodeled internal logging, metrics, allocation, caching, or tracing. Universal and keyless; may coexist with a keyed identity declared for `result_replay`'s benefit.

### 9.3 `result_replay` — terminal-result behaviour

- `unspecified` — no usable replay fact.
- `unstable` — an explicit negative: repeated applications are not guaranteed one replay-fixed terminal result; per-attempt results may differ (presigned URLs, fresh nonces, fresh challenges). Requires a declared result contract; requires no identity, since its very use is to state per-attempt freshness.
- `replay_stable` — requires `identity: keyed` and a declared result contract. **After one interaction's first terminal outcome, every later application of that identity observes the same terminal variant and a replay-equivalent payload.** Existing `ErrorDisposition` rules remain authoritative: `Ok` is terminal; `Err` contributes a fixed terminal result only under a declared `terminal` disposition; a retryable or disposition-unspecified `Err` is attempt-level and establishes nothing.

### 9.4 Independence

The three axes are orthogonal; Conseqa SHALL NOT derive either behavioural axis from the other — `identical_per_identity` ⇏ `replay_stable` and `replay_stable` ⇏ `identical_per_identity`. Canonical inhabitants:

```text
identity: unspecified  + side_effect_free       + unstable       (fresh presign)
identity: keyed        + identical_per_identity + replay_stable  (dedup provider replaying its recorded response)
identity: keyed        + side_effect_free       + replay_stable  (read-only stable snapshot per identity)
                         distinguishable                          (unkeyed charge)
```

## 10. Validation

New `ValidationError` variants:

- `ExternalIdempotencyRequiresIdentity` — `identical_per_identity` with `identity: unspecified`.
- `ExternalReplayStabilityRequiresIdentity` — `replay_stable` with `identity: unspecified`.
- `ExternalResultReplayWithoutResult` — `unstable` **or** `replay_stable` on a result-less external effect: there is no modeled synchronous result whose replay behaviour could be described.

`side_effect_free` and `distinguishable` impose no identity requirement.

## 11. Analyzer

### 11.1 Effect leg (`verification/idempotency.rs`, external arm at ~1346)

Consumes **only** `ExternalIdempotency` — `result_replay` plays no role in this proof leg:

- `SideEffectFree` → `EffectSafety::ExternallySideEffectFree` — duplicate-safe with no key condition.
- `IdenticalPerIdentity` → the existing key-stability check over the **identity** key → `EffectSafety::ExternallyIdempotent { identity_key }`; unstable roots → `ExternalIdentityKeyUnstable`.
- `Distinguishable` → obstacle `ExternalApplicationsDistinguishable`, evidence: *"declared `distinguishable`: a duplicate application may produce distinguishable modeled external work at that boundary."*
- `Unspecified` → `ExternalIdempotencyUnknown`.

(The current variants `ExternalEffectNotDeduplicated`, `ExternalEffectDeduplicationUnknown`, `ExternalDeduplicationKeyUnstable`, and `EffectSafety::ExternallyDeduplicated` are renamed accordingly; idempotency.rs:444,1051,1347–1380,1614.)

### 11.2 Rule 6 (`verification/replay.rs`, ~1575)

External result stability consumes **only** `ExternalIdentity`, `ExternalResultReplay`, `ResultType`, and `ErrorDisposition`. A bound external result is replay-stable for a governing class iff: `result_replay == ReplayStable`; the identity is keyed; every identity-key component is replay-stable under that class; and the observed variant is terminal. `ExternalIdempotency` plays no role — this separation is normative: a boundary may return stable results while its side-effect semantics are separately unsafe, and vice versa. The failure gap becomes `ResultGap::ExternalResultNotReplayStable` (rename at replay.rs:416), described (describe.rs:208): *"the external boundary declares no terminal-result replay guarantee, so no same-key terminal result is fixed."* Variant-terminality handling is unchanged.

### 11.3 Audit

Every consumer of `IdempotencyGuarantee` on an external effect is in `verification/idempotency.rs`, `verification/replay.rs`, and `verification/describe.rs`; the transaction consumers (replay.rs:1787 and elsewhere) are untouched. `recoverability.rs` flows through the shared `EffectSafety`/stability machinery — the audit confirms rather than assumes this.

## 12. Clean break

The parser accepts **only** the revised representation. Superseded external-effect syntax — `idempotency: {kind: deduplicated_by, key}`, `{kind: not_deduplicated}`, `{kind: unspecified}` — fails ordinary serde/schema validation like any other unknown shape. No legacy detection, compatibility parser, custom deserializer, automatic rewriting, or deprecated alias exists.

**Stored projects.** Workspace state is stored as `serde_json` of the typed model, guarded by the stored-workspace schema version (`FORMAT`, persistence.rs:47, currently 2). This revision **bumps `FORMAT` to 3**. A database written by format 2 is refused by version with the existing named error rather than surfacing the schema change as a corrupt value — the established mechanism: format 2 itself was introduced the same way, without migration, when the L1 runtime model arrived. Existing project stores are abandoned or re-authored; a model worth keeping is exported with a pre-revision binary (`export_spec`) and re-authored against the new schema.

---

# Part III — idempotency-inert continuation admissibility

## 13. The `IdempotencyInertContinuation` predicate

The idempotency control leg requires every decision on an admitted path to replay — generally correct, because divergent retries may follow divergent work histories. There is one narrow case where replay is unnecessary: if every continuation after the decision performs **no additional modeled work**, differing branch choices cannot create divergent side-effect histories; only terminal construction may differ, and result-replay semantics already govern that separate concern.

Define the analyzer predicate `IdempotencyInertContinuation(D)`: it holds iff every continuation from decision `D` to an operation terminal contains only

```text
branch    match_result    return    complete
```

— including nested decisions and **all fall-through control**: the suffix of a `branch` without `otherwise`, and the remaining steps of every enclosing block out to the operation terminal. The check applies to the entire continuation, never merely immediate arm bodies: a decision whose arms are clean but whose enclosing-block suffix contains `transaction T` is not inert; `if C { complete }` followed by `Effect E` is not inert, because the false-arm fall-through reaches `E`. The predicate is false if any continuation contains

```text
transaction
execute_effect            execute_effect_async
execute_effect_intent     execute_effect_intent_async
join_all                  race
```

or any future step capable of performing modeled state transformation, executing or launching an effect, or changing modeled effect-completion dependencies — a new step kind must decide its inertness here explicitly rather than inherit it (the `permits_direct_async` pattern).

**Transactions are always non-inert**, even when `deduplicated_by`, naturally replayable, or read-only in some particular proof: the theorem deliberately avoids recursively proving equivalence between different effectful continuations. `arm A → Transaction T1 / arm B → Transaction T2` remains an obstacle — different retries may perform different logical work.

**`join_all` and `race` are non-inert** although they launch nothing: they alter modeled completion dependencies, and with async effects already launched in the shared prefix, divergent synchronization may create observably different effect/control histories (`arm 1 → race(A,B) / arm 2 → complete`). Excluding them keeps the theorem simple and obviously sound. A launch *before* the decision does not by itself invalidate inertness; a launch *inside* a continuation (`execute_effect_async E; complete`) does — `E` enters the blast radius even unawaited. Outbox writes need no special rule: they exist only inside transactions, which are already non-inert.

## 14. The admissibility rule and its soundness

A decision not established to replay SHALL NOT block an **idempotency** proof when `IdempotencyInertContinuation(D)` holds.

Soundness: let two attempts share the governing class. They execute the same modeled prefix up to `D` (state and effect legs judge that prefix as before) and may take different outgoing paths. With every continuation inert, neither attempt performs any further transaction commit, effect execution, or async launch: the complete modeled work of both attempts is exactly the shared prefix's. The divergence can alter only which decisions are traversed, which terminal is reached, and which terminal result is constructed — and terminal-result divergence is governed by the result-replay obligation. Duplicate responses to a keyed request are collapsed by request identity (§8.1).

**Family confinement.** The theorem applies only to the idempotency family. It SHALL NOT apply to `ResultReplayRequirement::replay_consistent` — `arm A → return ResultA / arm B → return ResultB` may be idempotent (neither continuation performs work) while still failing result replay, and that is semantically correct: same work ≠ same result. The result-replay family continues to require the controlling decision itself to replay (result_replay.rs:147 untouched). Recoverability is likewise untouched — a progress obligation whose per-prefix analysis does not require decision replay today (`notify_gallery_published.recoverability` proves at revision 81 with the `unspecified` branch standing), and inert continuations contain no failing-prefix steps beyond terminal construction.

## 15. Derived proof evidence, not assumption

Acceptance under this theorem SHALL be recorded as a **derived structural proof fact**, never an implementation assumption:

```rust
ProofEvidence::DecisionDivergenceIdempotencyInert { location: ProgramLocation }
```

> *"decision at `<location>` is not established to replay; every continuation to a terminal is idempotency-inert, so divergence cannot add modeled work and may affect only terminal construction."*

The categorization is normative. The report's `assumptions` list conformance-conditional facts (§1.3) that a nonconforming implementation could violate; this fact was proved by the analyzer from program structure, no implementation claim supplied it, and if the program later gains an effectful step past the decision, the predicate ceases to hold and the ordinary obstacle returns automatically.

## 16. Analyzer implementation

At the emission site of the decision-replay obstacle in `verification/idempotency.rs` (behind the variant at ~425): evaluate `IdempotencyInertContinuation` first, reusing the `paths.rs` control-flow machinery; if false, emit the ordinary obstacle; if true, record the evidence and continue. The continuation scan MUST account for both `match_result` arms, `then`/`otherwise`, branch fall-through when `otherwise` is absent, nested decisions, and the suffixes of all enclosing blocks — never merely immediate arm bodies.

---

# Part IV — DSL contract versioning

## 17. Design

### 17.1 The version names the semantic contract, not the syntax

The DSL version is a single integer naming the **normative semantic contract** — the semantics document as a whole — not the parse schema. The distinction is live in this very revision: Part III changes what the analyzer proves with **zero syntax delta**; two builds could parse identical documents and return different verdicts. A version that only guarded syntax would let both call themselves the same DSL while disagreeing about what a model means. Therefore: **any normative change bumps the version** — vocabulary, validation, or proof semantics alike — and verdicts are relative to it. Purely internal changes (performance, diagnostics wording, refactors) do not bump.

**This revision declares `dsl: 1`.** Everything before it is unversioned prehistory: an artifact with no declared version predates versioning and is refused as such (§18.3), which is strictly better than the serde error it would otherwise produce. The version ledger lives in `CONSEQA_DSL_SEMANTICS.md` — a short table mapping each version to the revision specification that defined it — and the doc's header states the version it specifies.

### 17.2 One version per build; independent from `FORMAT`

A build speaks exactly one DSL version, compiled in as a constant (`DSL_VERSION: u64 = 1`). The stored-workspace `FORMAT` remains a **separate counter on a different axis**: `FORMAT` guards the storage encoding of workspace state (spec types *plus* drafts, commit records, task bookkeeping); the DSL version names the semantic contract. The invariant relating them: **a DSL bump forces a `FORMAT` bump** (stored workspaces embed spec types and verdict-relevant state), never conversely (a task-record change bumps `FORMAT` alone). The numbers are not aligned and must not be read as each other — at this revision, `dsl: 1` and `FORMAT: 3`.

### 17.3 Stamped, never authored

The model root is assembled server-side (`assemble_model`); authors write patches, not model documents. The DSL version is therefore **stamped by the server**, not hand-written: `Model` gains a leading field

```rust
pub struct Model {
    /// The DSL contract version this model is expressed in.
    /// Stamped at assembly; never authored.
    pub dsl: DslVersion,        // newtype over u64

    pub revision: Revision,
    // ... as before
}
```

and every export carries it as its first line (`dsl: 1` above `revision: 82`). `deny_unknown_fields` on `Model` means a *pre-versioning build* reading a versioned export fails loudly on the unknown field — acceptable: old builds are behind the break by definition.

## 18. Enforcement points

### 18.1 Documents: two-phase read

Any consumer of a whole specification document (a future import path, tooling, `conseqa-viz`) reads in two phases: first a **lenient version probe** — a `#[serde(default)]`-only struct reading nothing but `dsl: Option<u64>`, tolerating every other field — then dispatch:

- probe = build's version → full strict parse;
- probe = other version → refuse by name: *"specification declares dsl {found}, but this build reads dsl {DSL_VERSION}; the contract changed and specifications are not migrated automatically — re-author against the current DSL"* (`DslVersionMismatch { found }`, the FormatMismatch pattern lifted from the store to the document);
- probe = absent → refuse by name: *"specification declares no dsl version and predates versioning."*

The probe-then-dispatch shape has in-repo precedent (the `SelectorValue` key-peek visitor); the refusal wording has the `FormatMismatch` precedent (persistence.rs:57).

### 18.2 Patches: optional declaration, precise refusal

The `submit_patch` envelope gains an **optional** `dsl` field. Present and equal to the server's: accepted. Present and different: refused before shape-parsing with `DslVersionMismatch` — turning what an agent authored against a stale reference would experience as serde soup into one precise sentence. Absent: assumed current, since fanout workers learn the DSL from the live served guide and are version-synchronized by construction. The gate's mismatch message is the *only* place a wrong-version author needs to look.

### 18.3 Reports and served surfaces

- `requirement_report` and `spec_status` output gain a top-level `dsl: 1` — a verdict is a statement relative to a contract, and an archived report without the contract version is ambiguous (Part III makes this concrete: the same model text proves differently across the bump).
- The `dsl_guide` table of contents and `dsl_reference` header state the version they document, so an agent citing the guide cites a version.
- `list_projects` / `open_project` surface nothing new: within one store every model is the build's version by construction (§17.2); the stamp matters at the document boundary, not inside the store.

## 19. What Part IV is not

It is not migration (nothing rewrites old documents), not negotiation (no ranges, no minimum-supported window), and not multi-version hosting (§3). It is the naming discipline that makes the clean-break policy repeatable: v3 made breaks cheap; Part IV makes them *legible* — every future revision specification bumps `DSL_VERSION`, bumps `FORMAT`, and adds one row to the ledger, and every artifact that outlives a binary says which contract it speaks.

---

# Part V — MCP server version awareness

## 20. Three version axes, never conflated

An agent connected to the Conseqa MCP server sits at the intersection of three independent version numbers, and the server must keep them distinct:

| axis | what it versions | source | at this revision |
|---|---|---|---|
| **MCP protocol** | the wire handshake (rmcp `ProtocolVersion`) | the rmcp crate | rmcp default, unchanged |
| **server build** | the binary/crate (rmcp `Implementation` via `from_build_env`) | `CARGO_PKG_VERSION` | `conseqa` 0.1.0 |
| **DSL contract** | the normative semantics (Part IV `DSL_VERSION`) | compiled constant | `dsl: 1` |

These move at different rates: a bugfix bumps the build alone; an rmcp upgrade bumps the protocol alone; a normative semantics change bumps the DSL contract (and, per §17.2, `FORMAT`). The server SHALL NOT encode the DSL version into the `Implementation.version` field or the protocol version — that field honestly reports the build (`Implementation::from_build_env`, rmcp `ServerInfo::new` default, mcp.rs:1546), and overloading it would make "which server binary is this" and "which contract does it speak" the same unanswerable question the moment they diverge.

## 21. The handshake states the contract

The DSL version reaches the agent through the two surfaces the initialize handshake already carries, plus the guide it already serves — not a new field on `Implementation`:

1. **Instructions.** Both served briefs (`WORKER_INSTRUCTIONS`, `INTERACTIVE_INSTRUCTIONS`, mcp.rs:44,64) gain a leading line: *"This server speaks Conseqa DSL contract version 1; author against it, and learn it through `dsl_guide` and `dsl_reference`."* The instructions are the one text every agent reads at connect time regardless of backing, so the version is stated before the first patch is drafted. The worker brief already ends by pointing at the guide; now it names the version that guide documents.

2. **Guide and reference (machine-readable, no open project).** `dsl_guide` with no topic (the table of contents, `guide_toc`) and `dsl_reference` lead with `dsl: 1` — already in the §24 semantics-edit table, restated here as Part V's programmatic surface: these two tools need no project open, so a client can read the contract version immediately after initialize without selecting a project.

3. **Report and status (machine-readable, project open).** `requirement_report` and `spec_status` carry `dsl: 1` (Part IV §18.3). Together with (2) this means the version is readable both before and after a project is selected, by tool call, never only by parsing prose.

No new tool is introduced: the version rides surfaces that already exist. A dedicated `dsl_version` tool was considered and rejected — it would be the only tool whose answer is a compile-time constant, and (2) already answers the question without a project.

## 22. The connection declares; the document boundary refuses

The server SHALL NOT reject a client at initialize on any version axis. A connecting client is an **agent that adapts**, not a document that asserts a contract: it reads the server's stated version and authors against it. This mirrors §19 — refusal lives at the artifact boundaries (`submit_patch`'s optional `dsl`, §18.2; whole-document reads, §18.1), where a *stale authored claim* meets the server, not at the socket, where nothing has yet been claimed. An agent carrying a stale notion of the DSL in its own context is corrected the moment it submits: the patch envelope's version check answers, in one sentence, before shape-parsing.

This is precisely what backs Part IV §18.2's "version-synchronized by construction." The fanout runs workers against the **same daemon** that compiled `DSL_VERSION`; each worker's instructions state that version, each learns the DSL from that server's live `dsl_guide`, and each commits through that server's gate. There is no path by which a worker authors against a different contract than the one enforcing its commit — the synchronization is structural, and Part V is the structure. A worker started against a *different* server build is a deployment error outside the model's closed world (§1.3), not a case the protocol negotiates.

## 23. Analyzer and server changes

- A single crate constant `DSL_VERSION: u64 = 1` (beside the `Model.dsl` stamp of §17.3), referenced by the instructions builder, the guide/reference headers, and the report/status stamping — one source, every surface.
- `get_info` (mcp.rs:1545) interpolates `DSL_VERSION` into the instructions string for every backing; `Implementation` is left at `from_build_env`.
- `guide_toc` and the `dsl_reference` renderer prepend the version line.
- No `ServerCapabilities` change: version awareness is data the server states, not a capability it negotiates.

---

# 24. Semantics document edits

All in `CONSEQA_DSL_SEMANTICS.md`, which is `include_str!`-embedded (mcp.rs:1887) — these edits **are** the served `dsl_guide` updates; rebuild and restart the server to serve them.

| section | edit |
|---|---|
| document header | "This document specifies DSL contract version 1"; the version ledger table (version → revision specification) |
| §1 interpretation model | external boundaries declare three separate properties; mechanisms such as provider deduplication remain below the semantic floor |
| §2 model root | the `dsl` field: stamped at assembly, never authored; version semantics per Part IV |
| §1 / new subsection | the three version axes (MCP protocol, server build, DSL contract), their independence, and that the server states — never negotiates — the contract at the handshake (Part V) |
| §9 control leg (≈712) | the idempotency-inert continuation theorem; confinement to the idempotency family; evidence classified as analyzer-derived |
| §9 `result_replay` (≈716–730) | one sentence: the inert-continuation admission does not apply to this family |
| §13.3 external effects (≈1500–1525) | rewrite around `identity` / `idempotency` / `result_replay`; the old external idempotency vocabulary is removed entirely; interleaving-qualified `identical_per_identity`; modeled-scope `side_effect_free`; terminal-result text moves under `replay_stable` |
| §16 `branch` (≈1911–1938) | add `present` to the list and the determinism sentence; `eq`-over-absent; conditions never prune admitted paths; vacuous presence redundant-not-invalid |
| §18 rule 6 (≈2266) | re-gate on identity + `replay_stable` + stable identity key + terminal variant |
| §18 rule 8 (≈2278) | "external effect results outside rule 6 — an *undeclared* or unstably keyed boundary…" |
| §19 selectors | selector `eq`-over-absent sentence |
| §26 vocabulary table (≈2690) | rows: `present` vs `eq`; `identity`/`idempotency`/`result_replay` (external) vs `deduplicated_by` (transaction-only); `ExternalIdentityKey` vs `IdempotencyKey`; derived structural proof fact vs declared assumption; DSL version vs model `revision` vs stored `FORMAT` |

Rewrite the `dsl_reference` worked example in `mcp.rs` wherever it shows the old external surface. Update the `IdempotencyGuarantee` doc comment to transaction-only.

# 25. Tests

`tests/confluence_analysis.rs`, all through the commit gate:

**`present`** — presence/absence over an optional field; nested optional-segment absence; `not(present)`; replay from a replay-stable triggering payload (the `notify_gallery_published` shape flips `unknown → proven`, with the `condition: unspecified` variant as the unproven regression pair); `eq` with left/right/both absent; selector `eq` against an absent stored optional; `present` over a required path stays valid and emits only `RedundantPresenceCheck`.

**External guarantees** — the four §9.4 inhabitants prove/fail as specified; validation rejects `identical_per_identity` without identity, `replay_stable` without identity, and `unstable`/`replay_stable` without a result; retryable `Err` remains nonterminal under `replay_stable`; axis isolation both ways; keyed identity with both behaviours unspecified is valid and enables nothing; the superseded surface (`idempotency: {kind: deduplicated_by, …}`) fails ordinary deserialization — one test pinning the clean break.

**Idempotency-inert continuation** — admitted: non-replaying decision into `complete/complete`, `return A / return B`, and nested decisions over terminals only, each carrying the derived evidence; rejected: each excluded step kind in a continuation, a step appearing only in fall-through or enclosing-block suffixes, and a `branch` without `otherwise` whose fall-through is effectful; confinement: the same admitted decision still blocks a declared `result: replay_consistent` obligation.

**Versioning** — exports lead with `dsl: 1`; `requirement_report` and `spec_status` carry `dsl: 1`; the version probe refuses a document declaring `dsl: 2` and a document declaring none, each with its named error; a patch envelope declaring a mismatched `dsl` is refused with `DslVersionMismatch` before shape validation; an absent patch `dsl` is accepted.

**Server awareness** (`confluence_mcp` / `confluence_stdio`) — the initialize handshake's instructions state `dsl: 1` for both the worker and interactive backings; `Implementation.version` still reports the crate build, not the DSL version (the axes stay distinct); `dsl_guide` with no topic and `dsl_reference` lead with `dsl: 1` with no project open; initialize is never refused on a version axis.

`confluence_mcp` / `confluence_stdio`: one patch round-trip exercising the new external surface and `present`. Persistence: a format-2 database refuses to open with `FormatMismatch`.

# 26. Model re-authoring (pixieset-clone)

There is no migration stage. After the binaries ship, the format-3 server refuses the old store by version; the model is re-authored against the new schema from its export (taken with a pre-revision binary via `export_spec`; the revision-81 export already exists). The re-authored model differs from the export in exactly eight declarations:

1. `notify_gallery_published`: the step-1 branch condition becomes `not { present input.client_email }`.
2. `request_photo_upload.presign`: `identity: unspecified` + `side_effect_free` + `result_replay: unstable` — no invented deduplication identity; idempotency then proves through the inert-continuation theorem, and a hypothetical result-replay obligation would still fail, correctly.
3. The five `email_send` effects: `identity: keyed{notification_id}` + `identical_per_identity` + `replay_stable` — author-reviewed claims about the provider, not mechanical canonicalizations.
4. `process_photo.generate_variants`: the same triple keyed by `photo_id`, the content-addressing argument carried by the property rather than a collapse-mechanism claim.

**Acceptance**: `requirement_report` returns `all_proven: true` — 54 of 54 — stamped `dsl: 1`, with `DecisionDivergenceIdempotencyInert` evidence on `request_photo_upload.idempotency`, and no other verdict differs from revision 81. A fresh export leads with `dsl: 1`.

# 27. Sequencing

Three PRs, independently landable and green:

1. **Part II + Part IV + Part V** — the boundary decomposition together with `DSL_VERSION = 1`, the `Model.dsl` stamp, probe/refusal, report stamping, the handshake/instructions/guide version surfaces, and the `FORMAT` bump to 3. These travel together deliberately: the version machinery exists precisely to name this break, the server-awareness surfaces are how agents see that name, so the break, its name, and its announcement land in one release.
2. **Part I** — small and mechanically contained.
3. **Part III** — last, because it changes a verifier admissibility rule rather than adding vocabulary. (Landing after PR 1 means it ships inside `dsl: 1` before any artifact escapes; had it shipped separately later, it would bump the version alone, syntax-identical — the canonical example of §17.1.)

Model re-authoring (§26) follows the final binary rebuild and server restart. No migration stage exists or follows.
