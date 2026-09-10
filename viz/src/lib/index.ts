import type {
  Effect, Id, Model, Operation, OperationBlock, OperationStep, ResultType, Transaction,
  TransitionSideEffect,
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

/** One hop of a step location: the step's index in its block and, for
 *  every level but the last, the arm entered beneath it. */
export interface StepHop {
  step: number;
  arm?: "ok" | "err" | "then" | "otherwise";
}

/** A step location rendered as the checker names it: one-based, `3.ok.1`
 *  for the first step of the ok arm of the third top-level step. */
export function locationLabel(hops: StepHop[]): string {
  return hops.map((h) => `${h.step + 1}${h.arm ? `.${h.arm}` : ""}`).join(".");
}

export interface LocatedStep {
  location: string;
  hops: StepHop[];
  step: OperationStep;
}

/** Every step of a program with its location, depth first in program order. */
export function walkProgram(block: OperationBlock, parent: StepHop[] = []): LocatedStep[] {
  const out: LocatedStep[] = [];
  block.steps.forEach((step, index) => {
    const hops = [...parent, { step: index }];
    out.push({ location: locationLabel(hops), hops, step });
    const under = (arm: StepHop["arm"]) => [...parent, { step: index, arm }];
    if (step.kind === "match_result") {
      out.push(...walkProgram(step.ok, under("ok")));
      out.push(...walkProgram(step.err, under("err")));
    } else if (step.kind === "branch") {
      out.push(...walkProgram(step.then, under("then")));
      if (step.otherwise) out.push(...walkProgram(step.otherwise, under("otherwise")));
    }
  });
  return out;
}

/** Every inline transaction of an operation's program, in program order. */
export function operationTransactions(op: Operation): Transaction[] {
  return walkProgram(op.program).flatMap(({ step }) => (step.kind === "transaction" ? [step] : []));
}

/** The inline transaction with the given stable id. */
export function findTransaction(op: Operation, id: Id): Transaction | null {
  return operationTransactions(op).find((tx) => tx.id === id) ?? null;
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
      for (const inner of step.steps) {
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
        put(step.id, { kind: "transaction", op: opId });
        for (const inner of step.steps) {
          if (inner.kind === "establish_effect_intent") {
            put(inner.effect_id, { kind: "effect", op: opId });
            put(inner.bind, { kind: "intent", op: opId, effect: inner.effect_id, transaction: step.id });
          } else if (inner.kind === "write_outbox") {
            put(inner.effect_id, { kind: "effect", op: opId });
          } else if (inner.kind === "establish_transaction_output") {
            put(inner.bind, { kind: "output", op: opId, schema: inner.schema, transaction: step.id });
          } else if (inner.kind === "transition") {
            for (const [effectId, intent] of Object.entries(inner.effect_intents)) {
              put(intent.bind, {
                kind: "intent",
                op: opId,
                effect: effectId,
                transaction: step.id,
                via: { machine: inner.machine, transition: inner.transition },
              });
            }
          }
        }
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

  for (const [mId, m] of Object.entries(model.state_machines)) {
    for (const [tId, t] of Object.entries(m.transitions)) {
      for (const eId of Object.keys(t.side_effects)) {
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
    const effect =
      model.state_machines[owner.machine]?.transitions[owner.transition]?.side_effects[effectId];
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
      return `external ${e.name} · ${e.idempotency.kind}`;
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
