import type {
  DataObject, Effect, Id, Model, Operation, OperationBlock, OperationStep, ResultType, Transaction,
  TransactionStep, TransitionSideEffect,
} from "../types/model";
import { shortId } from "./ids";

/** Where an id is declared, resolved once for the whole model. Inline
 *  declarations — transactions, effects, intent and output bindings —
 *  are walked out of the operation programs, which are the source of
 *  truth for every operation-owned execution occurrence. */
export type IndexEntry =
  | { kind: "service" }
  | { kind: "operation" }
  | { kind: "schema" }
  | { kind: "topic" }
  | { kind: "data_model" }
  | { kind: "object"; dataModel: Id }
  | { kind: "outbox"; dataModel: Id }
  | { kind: "machine" }
  | { kind: "state"; machine: Id }
  | { kind: "transition"; machine: Id }
  | { kind: "input"; op: Id }
  | { kind: "effect"; op?: Id; machine?: Id; transition?: Id }
  /** An intent binding: `effect` is the captured effect occurrence,
   *  `transaction` the inline transaction that establishes it, and
   *  `via` the applied transition when the effect is transition-owned. */
  | { kind: "intent"; op: Id; effect: Id; transaction: Id; via?: { machine: Id; transition: Id } }
  /** A transaction-output binding with its declared schema and the
   *  inline transaction that establishes it. */
  | { kind: "output"; op: Id; schema: Id; transaction: Id }
  /** A transaction-local read binding: `step` is the 0-based index of
   *  the `read` step inside the inline transaction that binds it. Never
   *  available outside that transaction. */
  | { kind: "read"; op: Id; transaction: Id; step: number }
  /** A result binding declared by a program step; `effect` is what it observes. */
  | { kind: "binding"; op: Id; effect: Id; location: string }
  /** An async handle bound by a launch step: an operation-local
   *  synchronization artifact naming one in-flight execution of
   *  `effect`. `null` when the launched intent does not resolve. */
  | { kind: "handle"; op: Id; effect: Id | null; location: string }
  | { kind: "transaction"; op: Id }
  // L1 — the declared runtime realization. Indexed last, so an L0 id
  // always wins a collision: the application machine is what a reader
  // means by a bare id.
  | { kind: "pool" }
  | { kind: "router" }
  | { kind: "storage_layout" };

export type ModelIndex = Map<Id, IndexEntry>;

/** The arm of a decision a nested block belongs to, spelled as the
 *  checker spells it (`impl Display for Arm` in
 *  `src/spec/operation/program.rs`): the `ok` arm or an error-class arm
 *  `err:<class>` of a match, the `then` or `otherwise` arm of a branch,
 *  or the `rejected` block of a transaction step. */
export type Arm = "ok" | "then" | "otherwise" | "rejected" | `err:${string}`;

/** The arm of the named error class of a match. */
export function errArm(error: Id): Arm {
  return `err:${error}`;
}

/** One hop of a step location: the step's index in its block and, for
 *  every level but the last, the arm entered beneath it. */
export interface StepHop {
  step: number;
  arm?: Arm;
}

/** A step location rendered as the checker names it: one-based, `3.ok.1`
 *  for the first step of the ok arm of the third top-level step,
 *  `3.err:conflict.1` for an error arm, `3.rejected.1` for a rejection
 *  block. */
export function locationLabel(hops: StepHop[]): string {
  return hops.map((h) => `${h.step + 1}${h.arm ? `.${h.arm}` : ""}`).join(".");
}

export interface LocatedStep {
  location: string;
  hops: StepHop[];
  step: OperationStep;
}

/** Every step of a program with its location, depth first in program
 *  order — the arms of every decision and the rejection block of every
 *  rejectable transaction included. */
export function walkProgram(block: OperationBlock, parent: StepHop[] = []): LocatedStep[] {
  const out: LocatedStep[] = [];
  block.steps.forEach((step, index) => {
    const hops = [...parent, { step: index }];
    out.push({ location: locationLabel(hops), hops, step });
    const under = (arm: Arm) => [...parent, { step: index, arm }];
    if (step.kind === "transaction") {
      if (step.rejected) out.push(...walkProgram(step.rejected, under("rejected")));
    } else if (step.kind === "match_result") {
      out.push(...walkProgram(step.ok, under("ok")));
      for (const [error, arm] of Object.entries(step.errors)) {
        out.push(...walkProgram(arm, under(errArm(error))));
      }
    } else if (step.kind === "branch") {
      out.push(...walkProgram(step.then, under("then")));
      if (step.otherwise) out.push(...walkProgram(step.otherwise, under("otherwise")));
    }
  });
  return out;
}

/** One transaction execution site of a program: where it sits, the
 *  inline transaction, and the block control enters on rejection —
 *  present exactly when the body contains a rejecting step. */
export interface TransactionSite {
  location: string;
  transaction: Transaction;
  rejected: OperationBlock | null;
}

/** Every transaction execution site of an operation's program, in
 *  program order, rejection blocks and error arms included. */
export function transactionSites(op: Operation): TransactionSite[] {
  return walkProgram(op.program).flatMap(({ location, step }) =>
    step.kind === "transaction"
      ? [{ location, transaction: step.transaction, rejected: step.rejected ?? null }]
      : [],
  );
}

/** Every inline transaction of an operation's program, in program order. */
export function operationTransactions(op: Operation): Transaction[] {
  return transactionSites(op).map((site) => site.transaction);
}

/** A data object by id, with the data model that owns it. */
export function findDataObject(model: Model, id: Id): { dataModel: Id; object: DataObject } | null {
  for (const [dataModel, dm] of Object.entries(model.data_models)) {
    const object = dm.objects[id];
    if (object) return { dataModel, object };
  }
  return null;
}

/** The inline transaction with the given stable id. */
export function findTransaction(op: Operation, id: Id): Transaction | null {
  return operationTransactions(op).find((tx) => tx.id === id) ?? null;
}

/** The execution site of the inline transaction with the given id. */
export function findTransactionSite(op: Operation, id: Id): TransactionSite | null {
  return transactionSites(op).find((site) => site.transaction.id === id) ?? null;
}

/** Whether a transaction step is a logical commit guard that may reject
 *  the containing transaction: a transition (subject not in a `from`
 *  state), a version validation, a cursor advance, or a fence. */
/** Whether every path through the block ends at a terminal, by the
 *  validator's rule: the last step is a `return` or `complete`, or a
 *  decision whose every arm terminates (a branch needs an `otherwise`),
 *  or a transaction step whose rejected block terminates and that is
 *  followed by nothing — its committed path then falls through, so the
 *  block does not terminate. A block that does not terminate falls
 *  through to the join after the step that holds it. */
export function blockTerminates(block: OperationBlock): boolean {
  const last = block.steps[block.steps.length - 1];
  if (!last) return false;
  switch (last.kind) {
    case "return":
    case "complete":
      return true;
    case "match_result":
      return blockTerminates(last.ok) && Object.values(last.errors).every(blockTerminates);
    case "branch":
      return blockTerminates(last.then) && last.otherwise !== null && blockTerminates(last.otherwise);
    default:
      return false;
  }
}

export function stepRejects(step: TransactionStep): boolean {
  switch (step.kind) {
    case "transition":
    case "validate_version":
    case "advance_cursor":
    case "fence":
      return true;
    default:
      return false;
  }
}

/** Whether any step of the body may reject the transaction — exactly
 *  when its execution site must carry a `rejected` block. */
export function transactionRejects(tx: Transaction): boolean {
  return tx.steps.some(stepRejects);
}

/** Every operation-owned inline effect declaration with its id: direct
 *  execution sites, intent establishment sites, and transactional
 *  outbox-write sites, in program order. A write site's specific
 *  contract is surfaced through the effect union's `outbox_write`
 *  variant. */
export function operationEffects(op: Operation): [Id, Effect][] {
  const out: [Id, Effect][] = [];
  for (const { step } of walkProgram(op.program)) {
    if (step.kind === "execute_effect" || step.kind === "execute_effect_async") {
      out.push([step.effect_id, step.effect]);
    } else if (step.kind === "transaction") {
      for (const inner of step.transaction.steps) {
        if (inner.kind === "establish_effect_intent") out.push([inner.effect_id, inner.effect]);
        if (inner.kind === "write_outbox") {
          out.push([inner.effect_id, { kind: "outbox_write", ...inner.effect }]);
        }
      }
    }
  }
  return out;
}

export function buildIndex(model: Model): ModelIndex {
  const index: ModelIndex = new Map();
  const put = (id: Id, entry: IndexEntry) => {
    if (!index.has(id)) index.set(id, entry);
  };

  for (const id of Object.keys(model.services)) put(id, { kind: "service" });
  for (const id of Object.keys(model.schemas)) put(id, { kind: "schema" });
  for (const id of Object.keys(model.topics)) put(id, { kind: "topic" });

  for (const [dmId, dm] of Object.entries(model.data_models)) {
    put(dmId, { kind: "data_model" });
    for (const objId of Object.keys(dm.objects)) put(objId, { kind: "object", dataModel: dmId });
    for (const outboxId of Object.keys(dm.outboxes ?? {})) {
      put(outboxId, { kind: "outbox", dataModel: dmId });
    }
  }

  for (const [mId, m] of Object.entries(model.state_machines)) {
    put(mId, { kind: "machine" });
    for (const s of m.states) put(s, { kind: "state", machine: mId });
    for (const tId of Object.keys(m.transitions)) put(tId, { kind: "transition", machine: mId });
  }

  for (const [opId, op] of Object.entries(model.operations)) {
    put(opId, { kind: "operation" });
    for (const id of Object.keys(op.inputs)) put(id, { kind: "input", op: opId });

    // Inline declarations, walked out of the program: transactions,
    // effect occurrences, and the intent/output bindings their
    // producing sites introduce.
    for (const { step } of walkProgram(op.program)) {
      if (step.kind === "execute_effect" || step.kind === "execute_effect_async") {
        put(step.effect_id, { kind: "effect", op: opId });
      } else if (step.kind === "transaction") {
        const tx = step.transaction;
        put(tx.id, { kind: "transaction", op: opId });
        tx.steps.forEach((inner, i) => {
          if (inner.kind === "read") {
            put(inner.bind, { kind: "read", op: opId, transaction: tx.id, step: i });
          } else if (inner.kind === "establish_effect_intent") {
            put(inner.effect_id, { kind: "effect", op: opId });
            put(inner.bind, { kind: "intent", op: opId, effect: inner.effect_id, transaction: tx.id });
          } else if (inner.kind === "write_outbox") {
            put(inner.effect_id, { kind: "effect", op: opId });
          } else if (inner.kind === "establish_transaction_output") {
            put(inner.bind, { kind: "output", op: opId, schema: inner.schema, transaction: tx.id });
          } else if (inner.kind === "transition") {
            for (const [effectId, intent] of Object.entries(inner.effect_intents)) {
              put(intent.bind, {
                kind: "intent",
                op: opId,
                effect: effectId,
                transaction: tx.id,
                via: { machine: inner.machine, transition: inner.transition },
              });
            }
          }
        });
      }
    }

    // Result bindings and async handles second: an intent execution's
    // observed effect resolves through the intent binding registered
    // above, and a barrier's binding through the handle its launch
    // registered earlier in program order.
    for (const { location, step } of walkProgram(op.program)) {
      if (step.kind === "execute_effect" && step.bind) {
        put(step.bind, { kind: "binding", op: opId, effect: step.effect_id, location });
      } else if (step.kind === "execute_effect_intent" && step.bind) {
        const intent = index.get(step.intent);
        if (intent?.kind === "intent") {
          put(step.bind, { kind: "binding", op: opId, effect: intent.effect, location });
        }
      } else if (step.kind === "execute_effect_async") {
        put(step.handle, { kind: "handle", op: opId, effect: step.effect_id, location });
      } else if (step.kind === "execute_effect_intent_async") {
        const intent = index.get(step.intent);
        put(step.handle, {
          kind: "handle",
          op: opId,
          effect: intent?.kind === "intent" ? intent.effect : null,
          location,
        });
      } else if (step.kind === "join_all") {
        for (const entry of step.handles) {
          const handle = index.get(entry.handle);
          if (entry.bind && handle?.kind === "handle" && handle.effect) {
            put(entry.bind, { kind: "binding", op: opId, effect: handle.effect, location });
          }
        }
      } else if (step.kind === "race" && step.bind) {
        // The race result's possible producers are the whole candidate
        // set; the index attributes it to the first resolvable one,
        // whose contract every candidate is validated to share.
        for (const h of step.handles) {
          const handle = index.get(h);
          if (handle?.kind === "handle" && handle.effect) {
            put(step.bind, { kind: "binding", op: opId, effect: handle.effect, location });
            break;
          }
        }
      }
    }
  }

  // Transition-owned effects: the side effects an application binds as
  // intents, and the outbox admissions it makes atomically.
  for (const [mId, m] of Object.entries(model.state_machines)) {
    for (const [tId, t] of Object.entries(m.transitions)) {
      for (const eId of Object.keys(t.side_effects)) {
        put(eId, { kind: "effect", machine: mId, transition: tId });
      }
      for (const eId of Object.keys(t.effects ?? {})) {
        put(eId, { kind: "effect", machine: mId, transition: tId });
      }
    }
  }

  const runtime = model.runtime ?? {};
  for (const id of Object.keys(runtime.execution_pools ?? {})) put(id, { kind: "pool" });
  for (const id of Object.keys(runtime.routers ?? {})) put(id, { kind: "router" });
  for (const id of Object.keys(runtime.storage_layouts ?? {})) put(id, { kind: "storage_layout" });

  return index;
}

export interface EffectDef {
  effect: Effect | TransitionSideEffect;
  owner: Extract<IndexEntry, { kind: "effect" }>;
}

export function effectDef(model: Model, index: ModelIndex, effectId: Id): EffectDef | null {
  const owner = index.get(effectId);
  if (!owner || owner.kind !== "effect") return null;
  if (owner.op !== undefined) {
    const op = model.operations[owner.op];
    const found = op ? operationEffects(op).find(([id]) => id === effectId) : undefined;
    return found ? { effect: found[1], owner } : null;
  }
  if (owner.machine !== undefined && owner.transition !== undefined) {
    const transition = model.state_machines[owner.machine]?.transitions[owner.transition];
    const effect = transition?.side_effects[effectId] ?? transition?.effects?.[effectId];
    return effect ? { effect, owner } : null;
  }
  return null;
}

export function effectSummary(model: Model, index: ModelIndex, effectId: Id): string {
  const def = effectDef(model, index, effectId);
  if (!def) return "unresolved effect";
  const e = def.effect;
  switch (e.kind) {
    case "publication":
      return `publish ${shortId(e.schema)} → ${shortId(e.topic)}`;
    case "request":
      return `request ${shortId(e.target.operation)} (${shortId(e.target.input)}) · retry ${e.retry}`;
    case "external":
      return `external ${e.name} · ${e.idempotency}`;
    case "outbox_write":
      return `write ${shortId(e.schema)} → outbox ${shortId(e.outbox)}`;
  }
}

/** The `Result<Ok, Err>` contract an effect's execution yields: a
 *  request inherits its target input's, an external effect declares its
 *  own, a publication has none. */
export function effectResultType(model: Model, index: ModelIndex, effectId: Id): ResultType | null {
  const def = effectDef(model, index, effectId);
  if (!def) return null;
  const e = def.effect;
  switch (e.kind) {
    case "publication":
    case "outbox_write":
      return null;
    case "external":
      return e.result;
    case "request": {
      const input = model.operations[e.target.operation]?.inputs[e.target.input];
      return input?.kind === "request" ? input.result : null;
    }
  }
}

/** Operations that bind an intent capturing an effect, with the binding:
 *  transition applications binding a transition-owned side effect, and
 *  explicit establishment sites capturing an operation-owned one. */
export function intentExecutors(model: Model, effectId: Id): { op: Id; intent: Id }[] {
  const out: { op: Id; intent: Id }[] = [];
  for (const [opId, op] of Object.entries(model.operations)) {
    for (const tx of operationTransactions(op)) {
      for (const inner of tx.steps) {
        if (inner.kind === "establish_effect_intent" && inner.effect_id === effectId) {
          out.push({ op: opId, intent: inner.bind });
        } else if (inner.kind === "transition") {
          const intent = inner.effect_intents[effectId];
          if (intent) out.push({ op: opId, intent: intent.bind });
        }
      }
    }
  }
  return out;
}
