# Conseqa Retryable Error Propagation Revision

**Status:** implemented 2026-10-01 on branch `retryable-error-propagation`: phase A `951f182`, phase B `4213fd0`, phase C in the commit that adds this document. Two decisions made while implementing are recorded in C6.
**Base:** DSL 4, report format 7, storage format 6, branch `system-one-harness-revision` at `a0b7989`.
**Proposed:** DSL 5, report format 8, storage format 7.

## 0. Goal

A system must be able to pass a retryable error up any chain of operations, from the external boundary that produced it to the client that started the work. The goal is that every operation along the way still proves its idempotency, result replay, and recoverability obligations. In the model, "passing it up" means one of two things:

- **A request handler** returns an error class its own contract declares `retryable`. The caller observes an attempt-level outcome and may try again.
- **A message consumer** (a subscription or outbox input) ends the attempt without completing. The message stays unacknowledged or pending, so it is delivered again.

The first is already expressible. The second is not. This revision also closes a soundness hole that the first one exposes.

## 1. Findings

Each finding was verified against the tree.

**F1. Request chains already pass retryable errors up.** Say `transfer_stock` charges an external boundary whose `declined` class is `retryable` and returns its own `retryable` `rejected` on that arm, and `rebalance` requests `transfer_stock` and passes `rejected` on in turn. At both hops, idempotency and result replay are proven. Each retryable return is exempt from result replay (`RetryableReturn`, `result_replay.rs`). The decision on it only returns, so idempotency's inert-continuation admission covers it. The test `a_retryable_error_bubbles_up_a_request_chain_and_every_hop_still_replays` (`tests/verification.rs`) records this.

**F2. Soundness hole: a target's retryable error is treated as replay-stable.** §18 rule 6 says a request result's variants are "stable at once" when the target proves `result: replay_consistent`. The comment on `BoundResult` (`replay.rs:456`) says the same: "a request result carries one judgment in every arm". That was true until DSL 4 (`2b4e82e`, on master) exempted retryable returns from the target's result-replay obligation. Since then, a target's proof no longer fixes its retryable variants, but callers still treat them as fixed.

- **Counterexample.** Take F1's chain, but have `rebalance` map the target's retryable `rejected` to its own `rejected` declared `terminal`. Attempt 1: the boundary throttles, `transfer_stock` returns `rejected`, and `rebalance` returns the terminal `rejected`. Attempt 2: the boundary succeeds, and `rebalance` returns `Ok`. One logical request resolves terminally twice, with different results. The checker reports `rebalance`'s result replay as **proven** (`ReplayConsistentTarget` cited on the `Err` arm). This was reproduced on the tree.
- **The same hole in idempotency.** The control leg accepts a decision on a target's retryable arm as replaying, so effectful work in that arm is judged as if every attempt took the arm.

**F3. Once F2 is fixed, the most common pattern stops proving.** Consider `match` on a result where `ok` does work (a transaction or an effect) and `throttled` passes the error up. The `throttled` arm's decision is rightly not established to replay. The inert-continuation admission (§9 control leg; `idempotency_inert_decisions`, `idempotency.rs:1102`) is then the only way through. But it is quantified per decision *location*: "a location effectful on any one continuation is disqualified on all". The `ok` arm's work therefore disqualifies the `throttled` arm, although that arm adds no work. External boundaries hit this today (`ExternalErrorRetryable`); after F2, internal targets hit it too.

**F4. A message consumer cannot pass anything up.** Its only terminal is `complete`, and reaching `complete` is successful logical completion. That acknowledges a subscription message (when `acknowledge_on_success` is declared) and consumes an outbox message (§8.2, §8.3). So a consumer that observes a retryable error must either swallow it (complete, and the message is gone) or do failure work in the error arm. The `video_streaming` fixture header documents the consequence: "Were the engine's error retryable … the transcoder's idempotency … would be unknown".

**F5. External `replay_stable` is stricter than internal result replay.** §13.3 says: "after one interaction's first terminal outcome, every later application of that identity observes the same terminal variant". That forbids a boundary that throttles a later attempt after it has already succeeded, which rate limiters commonly do before their deduplication lookup. An internal operation proving `replay_consistent` may already do exactly that, because its retryable returns are exempt. Both are boundaries of one logical interaction, but they carry different contracts.

## 2. Changes

### C1. A retryable target error is never a stable result (fixes F2)

§18 rule 6 for a **request** becomes per variant, like the external half:

> The instance, schema, and proven-target conditions are unchanged. Given them, `Ok` is stable. An error class is stable iff the target's contract for the targeted input does **not** declare it `retryable`. A `terminal` class is fixed by the target's proof. So is an `unspecified` one, because the target's result-replay obligation holds unspecified classes to the obligation like `Ok` (only retryable returns are exempt). A `retryable` class is never stable: the target's proof says nothing about it.

- New gap: `ResultGap::TargetErrorRetryable { operation, input, error }`.
- `BoundResult` stops carrying one judgment for all arms of a request result.
- `effect_result_err` roots of a retryable class become `Unknown` (rule 8).
- §16 "Decision replay" lists the new gap beside `ExternalErrorRetryable`.

This is a pure soundness fix: it can only turn proven verdicts into unproven ones, and every such flip was a false proof. It is independent of everything else here and can ship on its own (§4, phase A), as PR #32 did.

### C2. The inert-continuation admission is scoped to the path (fixes F3)

The §9 control leg's admission is restated per path:

> A decision not established to replay on path *p* is admissible for this leg iff, on *p*, every step after it is itself a decision, out to *p*'s terminal.

The derived fact keeps its name, `IdempotencyInertContinuation`, and is recorded on the path's proof. Nothing else about the admission changes: `join_all` and `race` are never inert, a launch inside the continuation disqualifies it, and transactions are never inert.

*Why it is sound.* Fix a class and a decision *d*. Each attempt takes one path through *d*.

- An attempt whose path is unstable at *d* and inert after it does only the work of the shared prefix. The prefix is on that path, so the state and effect legs judge it there.
- An attempt whose path is stable at *d* takes the arm that the decision-replay rules fix for every attempt observing that variant. Its continuation is judged on its own path.
- Every decision-replay rule (§16) is **terminal-unique**: at most one stably-taken arm of a decision occurs in a class. A stable `branch` has one predicate value. A stable `match_result` rests on a fixed terminal outcome, and a keyed transaction re-takes its committed outcome.
- So the class's work is the prefix, repeated and judged retry-safe, plus at most one effectful continuation, also judged retry-safe. Nothing diverges.

The old location-wide quantification guarded against `arm A → T1 / arm B → T2`. That case is still an obstacle: whichever of A or B is unstable on its own path has an effectful continuation there. If both are stable, terminal uniqueness keeps them from both occurring.

Terminal uniqueness becomes a stated invariant of §16: **any future decision-replay rule must preserve it**.

Result replay gets no such admission, unchanged: divergent terminals construct divergent results. Retryable returns stay exempt there, as before. `OutcomeDivergenceAddsNoWork` is unchanged.

### C3. A non-completing terminal for message consumers: `abandon` (fixes F4)

```yaml
- kind: abandon
```

**Meaning.** `abandon` conclusively ends the current attempt **without** successful logical completion. It is the message-input counterpart of returning a `retryable` error:

- For a subscription, no acknowledgement takes effect, so the message remains logically unacknowledged.
- For an outbox, the message remains pending.
- Another attempt is semantically admitted. Whether one occurs is the driver's business: subscription delivery semantics (§10.3), or the outbox's intrinsic re-drive (§8.3).
- Like `retryable`, it does not say that a retry occurs, succeeds, or happens promptly.
- `abandon` rolls nothing back: committed transactions stay committed, and executed effects stay executed. It is a terminal, not a compensation.

**Program shape.** `abandon` is a terminal like `return` and `complete`. It ends its block, carries no payload, and names no input. It is legal anywhere a terminal is: in a block, a `match_result` arm, a `branch` arm, or a `rejected` block.

**Admission (§16).** A path ending at `abandon` is admitted for every subscription and outbox input of the operation, and for no request input. A request-triggered invocation passes a retryable error up with `return`.

**Validation.**
- `AbandonWithoutMessageInput` (error): the program contains `abandon` and the operation declares no subscription or outbox input.
- `AbandonWithoutRedelivery` (warning, L1): a subscription input of the operation has a runtime declaring `delivery: at_most_once`. An abandoned message is then never delivered again, so `abandon` drops it.

**Idempotency.** Abandoning paths are admitted paths, and all three legs apply unchanged. There is no final-step exemption, because the re-driven attempt re-encounters every transaction on the path. A decision whose continuation on the abandoning path is only decisions is admissible under C2. The `at_most_once` vacuity rule is unchanged. An abandoning attempt is followed by another attempt in the same class whenever the driver re-drives, so retry safety is mandatory on every step before `abandon`.

**Result replay.** Not applicable: abandoning paths return no result, and the obligation is vacuous for message inputs.

**Recoverability.** `abandon` is not a valid terminal of the logical invocation, so reaching it does not discharge the obligation. Same-path continuation (§9) treats an abandoning path as an interruption at its end:

- every transaction on it must resolve on re-encounter, with no final-step exemption;
- consumed artifacts are judged as usual;
- the obligation is discharged by the input's non-abandoning admitted paths, judged as today.

A new obstacle, `EveryPathAbandons`, applies when every admitted path for the input ends at `abandon`. Progress is then impossible by construction.

For `completion: guaranteed`, the driver facts are unchanged: `at_least_once` delivery for a subscription, and intrinsic re-drive for an outbox. The obligation remains conditional on the driver genuinely re-driving (§9, first caution). An abandoning path is exactly the case that caution covers.

**Serializability and ordering.** Unaffected: they judge committed histories, and `abandon` commits nothing. Whether a runtime holds later messages of an ordered scope behind an abandoned one is dispatch (§10.3.1), not program semantics.

### C4. External `replay_stable` admits a later retryable error (fixes F5)

§13.3's `replay_stable` becomes:

> After one interaction's first terminal outcome, every later application of that identity observes either the same terminal variant with a replay-equivalent payload, or an `Err` of a class the contract declares `retryable`.

This weakens the boundary's conformance obligation, so every existing declaration remains conforming. No proof weakens. Rule 6 already judges external results per variant, retryable variants are never stable, and C2's argument consumes only terminal uniqueness, which the new wording keeps. Internal targets and external boundaries now share one contract: a fixed terminal outcome, with attempt-level retryable errors around it.

### C5. Authoring surface

- **Sketch vocabulary:** a new `abandon` step, usable inside `when` and inside the `on_error` of `call` and `request`. The compiler refuses `abandon` in an operation with no message input, with the same reason as the validation rule.
- **`DSL_REFERENCE` and `dsl_guide`:** document `abandon` and the pattern for passing a retryable error up (a request returns its own retryable class; a consumer abandons).
- **Fixture:** `video_streaming`'s header is corrected, and a variant is added in which the engine's error is retryable and the transcoder abandons.
- **Harness:** the System One builders need no new questions, since `abandon` is the author's explicit statement in the sketch.

### C6. Versions

| | from | to | why |
|---|---|---|---|
| DSL | 4 | 5 | the `abandon` terminal |
| report format | 7 | 8 | `TargetErrorRetryable`, per-path `IdempotencyInertContinuation`, `EveryPathAbandons`, the two diagnostics |
| storage format | 6 | 7 | the persisted terminal enum gains a variant |

DSL 4 models parse unchanged under DSL 5. This is the first additive revision, so the build reads both: `DSL_READS = [4, 5]`. A `dsl: 4` document is read and re-emitted as `dsl: 5`, and `submit_patch` accepts either declaration. For the same reason, a format-6 store is upgraded in place (`UPGRADES_FROM = [6]`), so existing projects keep working. A format-6 build still refuses a format-7 store, which may hold `abandon`.

Their verdicts can change in two directions:

- **Flips to unproven (C1):** each was a false proof, and its obstacle names the gap.
- **Flips to proven (C2):** patterns that were conservatively refused.

The migration note lists both, and the fixtures' report snapshots are regenerated with the change explained in each.

## 3. Tests

- **C1:** F2's counterexample is unproven, with `TargetErrorRetryable`. F1's chain stays proven at both hops. A target's `unspecified` error stays stable. An `effect_result_err` root of a retryable class is `Unknown`.
- **C2:** `ok → transaction; throttled → return retryable` proves idempotency, citing `IdempotencyInertContinuation` on the throttled path only. `arm A → T1 / arm B → T2` with an unstable decision is still an obstacle. An inert path beside an unstable effectful path is still an obstacle on the latter. A `race`-bound decision is never admitted.
- **C3:**
  - `abandon` in a request-only operation → `AbandonWithoutMessageInput`.
  - Under `at_most_once` → `AbandonWithoutRedelivery`.
  - A subscription consumer `call (retryable) → ok: keyed transaction, complete; throttled: abandon` proves idempotency and `guaranteed` recoverability under `at_least_once`.
  - The same consumer with a non-keyed transaction before `abandon` is refused on the state leg.
  - Every path abandoning → `EveryPathAbandons`.
  - An outbox consumer abandoning proves `guaranteed` by intrinsic re-drive (`l0_only`).
  - Parser, canonical serialization, and storage round-trips.
- **C4:** no verdict change on any fixture.
- **C5:** the sketch path compiles `abandon`, and the coverage assertion `every_dsl_construct_is_reachable_from_a_sketch` includes it.

## 4. Delivery

- **A. C1 alone, against master.** A soundness fix, report format 8 for the new gap, and fixture snapshots regenerated. Ship it first, as its own PR.
- **B. C2 and C4.** These restore what A takes away for the common pattern. C4 is documentation plus the conformance wording; C2 is roughly forty lines in `idempotency.rs`.
- **C. C3 and C5.** The DSL 5 bump: spec, parser, validation, paths, idempotency, recoverability, viz terminal glyph, sketch, references, and fixture.

A and B touch only the analyzer and the semantics document. C touches the whole stack.

## 5. Open questions

1. **The name.** `abandon` was chosen over:
   - `fail`, which reads as terminal failure;
   - `retry`, which asserts that a retry occurs, the very thing `retryable` declines to assert;
   - `nack` and `release`, which name transport mechanics below the semantic floor (§1).
2. **A reason payload on `abandon`.** Proposed: no. No model construct would consume it, and observability is below the floor. A later revision may add one with a schema if something consumes it.
3. **Per-input admission.** `abandon` reuses §16's terminal-based admission. An operation with both a request and a message input therefore cannot say that only its message input abandons, beyond the request paths never reaching `abandon`. This is open question 10 (§27), unchanged.
4. **An abandon budget.** Repeated abandonment of one message (a poison message) is a liveness concern that `guaranteed` already leaves conditional. Proposed: no attempt counts, consistent with V1's refusal to model retry policy (§27, retry execution).
