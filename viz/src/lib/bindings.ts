// The names a program introduces and where it consumes them.
//
// The DSL has exactly five kinds of binding — a name one step introduces
// for later steps — and everything else that looks like a name is not
// one: execution-site ids (`tx.x`, `effect.x`), inputs, schemas, objects.
// This module walks one operation's program once and answers, for every
// binding, where it is defined and where it is used, so the views can
// draw a name the same way at both ends.

import type {
  Condition, Derivation, Effect, FieldPath, Id, IdempotencyKey, IdempotencyKeyPropagation, Model,
  ObjectSelector, Operation, OperationStep, OutboxWriteEffect, ResultVariant, SelectorPredicate,
  SelectorValue, Transaction, ValueRef,
} from "../types/model";
import { walkProgram } from "./index";

/** The five kinds of binding, by what introduces them:
 *  - `read`: a transaction `read` step; transaction-local.
 *  - `output`: `establish_transaction_output`; program, committed path.
 *  - `intent`: `establish_effect_intent` or a transition step's
 *    `effect_intents`; program, committed path.
 *  - `result`: `execute_effect`, `execute_effect_intent`, a `join_all`
 *    entry, or `race`; program, from the step on.
 *  - `handle`: `execute_effect_async` or `execute_effect_intent_async`;
 *    program, from the launch on. */
export type BindingKind = "read" | "output" | "intent" | "result" | "handle";

/** The card that produces a binding, as the operation page keys its
 *  selection: a transaction card, a direct effect execution or launch, an
 *  intent execution or launch, or an anonymous step (a barrier). */
export type ProducerSite = "transaction" | "effect" | "intent" | "step";

export interface BindingDef {
  name: Id;
  kind: BindingKind;
  op: Id;
  /** Program step location of the producing step: `1`, `2.ok.1`. For a
   *  transaction-scoped binding, the transaction step's location. */
  location: string;
  /** The inline transaction that binds it — read, output, intent. */
  transaction?: Id;
  /** 0-based index of the binding step inside the transaction. */
  txStep?: number;
  scope: "transaction" | "program";
  /** Short human label of the producer: `read of object.order`,
   *  `execute effect effect.x`, `transition transition.order.mark_paid`,
   *  `join_all`, `race`, `launch of effect.x`. */
  producer: string;
  site: ProducerSite;
  /** The effect the producing card executes, launches, or captures. */
  effect?: Id;
  /** The intent the producing card executes or launches. */
  intent?: Id;
}

/** How a step consumes a binding. */
export type BindingUseHow =
  /** A root of a derivation: a write, insert, establishment, effect
   *  instance, transition intent or admission, or return payload. */
  | "value source"
  | "match"
  | "execute intent"
  | "join"
  | "race"
  | "guard expected"
  | "cursor position"
  | "fence token"
  | "requirement key"
  | "ordering position"
  | "selector"
  | "condition"
  /** A component of a transaction's commit-deduplication key. */
  | "commit key"
  /** A component of an effect's idempotency key propagation. */
  | "key propagation"
  /** A component of an external effect's interaction identity key. */
  | "identity key";

export interface BindingUse {
  name: Id;
  /** Program step location of the consuming step; for a use inside a
   *  transaction body, the transaction step's location. */
  location: string;
  /** 0-based index of the consuming step inside the transaction, when
   *  the use is inside a transaction body. Absent for a transaction's
   *  own facts — its commit key and requirements. */
  txStep?: number;
  how: BindingUseHow;
  /** For a value reference, the field path read off the binding. */
  path?: FieldPath;
  /** For a result reference, which arm's payload it reads. */
  arm?: ResultVariant;
}

export interface OperationBindings {
  /** Every binding the program defines, in program order. */
  defs: Map<Id, BindingDef>;
  /** Every use, by bound name, in program order. A name may be used
   *  without being defined here — an unresolved intent, say. */
  uses: Map<Id, BindingUse[]>;
}

/** The binding a value reference reads, or null when its source is not
 *  a binding: an input, an effect's own fields, a machine subject. */
export function refBinding(ref: ValueRef): { name: Id; kind: BindingKind; arm?: ResultVariant } | null {
  switch (ref.source.kind) {
    case "transaction_read":
      return { name: ref.source.id, kind: "read" };
    case "transaction_output":
      return { name: ref.source.id, kind: "output" };
    case "effect_result_ok":
      return { name: ref.source.id, kind: "result", arm: "ok" };
    case "effect_result_err":
      return { name: ref.source.id, kind: "result", arm: "err" };
    case "input":
    case "effect":
    case "state_machine_subject":
      return null;
  }
}

export function operationBindings(opId: Id, op: Operation): OperationBindings {
  const defs = new Map<Id, BindingDef>();
  const uses = new Map<Id, BindingUse[]>();

  const def = (d: BindingDef) => {
    if (!defs.has(d.name)) defs.set(d.name, d);
  };
  const use = (u: BindingUse) => {
    const list = uses.get(u.name);
    if (list) list.push(u);
    else uses.set(u.name, [u]);
  };

  // Every ValueRef site, then the structures that hold them.
  const ref = (r: ValueRef, location: string, how: BindingUseHow, txStep?: number) => {
    const b = refBinding(r);
    if (b) use({ name: b.name, location, txStep, how, path: r.path, arm: b.arm });
  };
  const derivation = (d: Derivation, location: string, txStep?: number) => {
    if (d.kind === "deterministic") for (const r of d.from) ref(r, location, "value source", txStep);
  };
  const key = (k: IdempotencyKey, location: string, how: BindingUseHow, txStep?: number) => {
    for (const r of k.components) ref(r, location, how, txStep);
  };
  const propagation = (items: IdempotencyKeyPropagation[], location: string, txStep?: number) => {
    for (const p of items) {
      key(p.source, location, "key propagation", txStep);
      key(p.target, location, "key propagation", txStep);
    }
  };
  const effect = (e: Effect | OutboxWriteEffect, location: string, txStep?: number) => {
    if ("idempotency_key_propagation" in e) propagation(e.idempotency_key_propagation, location, txStep);
    if ("identity" in e && e.identity.kind === "keyed") {
      for (const r of e.identity.key.components) ref(r, location, "identity key", txStep);
    }
  };
  const selectorValue = (v: SelectorValue, location: string, how: BindingUseHow, txStep?: number) => {
    if (v.kind === "value") ref(v.value, location, how, txStep);
  };
  const predicate = (p: SelectorPredicate, location: string, txStep?: number) => {
    switch (p.kind) {
      case "all":
        return;
      case "eq":
        selectorValue(p.value, location, "selector", txStep);
        return;
      case "and":
        for (const inner of p.predicates) predicate(inner, location, txStep);
        return;
    }
  };
  const selector = (s: ObjectSelector, location: string, txStep?: number) => predicate(s.predicate, location, txStep);
  const condition = (c: Condition, location: string) => {
    switch (c.kind) {
      case "unspecified":
        return;
      case "eq":
        ref(c.value, location, "condition");
        selectorValue(c.equals, location, "condition");
        return;
      case "and":
        for (const inner of c.conditions) condition(inner, location);
        return;
      case "not":
        condition(c.condition, location);
        return;
      case "present":
        ref(c.value, location, "condition");
        return;
    }
  };

  const transaction = (tx: Transaction, location: string) => {
    const scoped = { op: opId, location, transaction: tx.id, site: "transaction" as const };

    if (tx.idempotency.kind === "deduplicated_by") key(tx.idempotency.key, location, "commit key");
    for (const r of tx.requirements.serializability) ref(r.key, location, "requirement key");
    for (const r of tx.requirements.ordering) {
      ref(r.key, location, "requirement key");
      ref(r.position, location, "ordering position");
    }

    tx.steps.forEach((step, txStep) => {
      switch (step.kind) {
        case "read":
          def({ ...scoped, name: step.bind, kind: "read", txStep, scope: "transaction", producer: `read of ${step.target.object}` });
          selector(step.target, location, txStep);
          return;
        case "write":
          selector(step.target, location, txStep);
          derivation(step.values, location, txStep);
          return;
        case "insert":
          derivation(step.values, location, txStep);
          return;
        case "delete":
        case "lock":
        case "bump_version":
          selector(step.target, location, txStep);
          return;
        case "transition":
          selector(step.subject, location, txStep);
          for (const [effectId, intent] of Object.entries(step.effect_intents)) {
            def({
              ...scoped, name: intent.bind, kind: "intent", txStep, scope: "program",
              producer: `transition ${step.transition}`, effect: effectId,
            });
            derivation(intent.values, location, txStep);
          }
          for (const application of Object.values(step.effects ?? {})) derivation(application.values, location, txStep);
          return;
        case "establish_effect_intent":
          def({
            ...scoped, name: step.bind, kind: "intent", txStep, scope: "program",
            producer: `establish intent for ${step.effect_id}`, effect: step.effect_id,
          });
          derivation(step.values, location, txStep);
          effect(step.effect, location, txStep);
          return;
        case "establish_transaction_output":
          def({ ...scoped, name: step.bind, kind: "output", txStep, scope: "program", producer: `establish output ${step.schema}` });
          derivation(step.values, location, txStep);
          return;
        case "write_outbox":
          derivation(step.values, location, txStep);
          effect(step.effect, location, txStep);
          return;
        case "validate_version":
          selector(step.target, location, txStep);
          ref(step.expected, location, "guard expected", txStep);
          return;
        case "advance_cursor":
          selector(step.target, location, txStep);
          ref(step.incoming, location, "cursor position", txStep);
          return;
        case "fence":
          selector(step.target, location, txStep);
          ref(step.token, location, "fence token", txStep);
          return;
      }
    });
  };

  // Depth first in program order — every arm, every rejected block — so
  // a barrier's binding can resolve the handle its launch defined
  // earlier.
  for (const { location, step } of walkProgram(op.program)) {
    const program = { op: opId, location, scope: "program" as const };
    switch (step.kind) {
      case "transaction":
        transaction(step.transaction, location);
        break;
      case "execute_effect":
        derivation(step.values, location);
        effect(step.effect, location);
        if (step.bind) {
          def({ ...program, name: step.bind, kind: "result", producer: `execute effect ${step.effect_id}`, site: "effect", effect: step.effect_id });
        }
        break;
      case "execute_effect_async":
        derivation(step.values, location);
        effect(step.effect, location);
        def({ ...program, name: step.handle, kind: "handle", producer: `launch of ${step.effect_id}`, site: "effect", effect: step.effect_id });
        break;
      case "execute_effect_intent":
        use({ name: step.intent, location, how: "execute intent" });
        if (step.bind) {
          def({ ...program, name: step.bind, kind: "result", producer: `execute intent ${step.intent}`, site: "intent", intent: step.intent });
        }
        break;
      case "execute_effect_intent_async":
        use({ name: step.intent, location, how: "execute intent" });
        def({ ...program, name: step.handle, kind: "handle", producer: `launch of intent ${step.intent}`, site: "intent", intent: step.intent });
        break;
      case "join_all":
        for (const entry of step.handles) {
          use({ name: entry.handle, location, how: "join" });
          if (entry.bind) {
            def({ ...program, name: entry.bind, kind: "result", producer: "join_all", site: "step", effect: defs.get(entry.handle)?.effect });
          }
        }
        break;
      case "race":
        for (const h of step.handles) use({ name: h, location, how: "race" });
        if (step.bind) def({ ...program, name: step.bind, kind: "result", producer: "race", site: "step" });
        break;
      case "match_result":
        use({ name: step.result, location, how: "match" });
        break;
      case "branch":
        condition(step.condition, location);
        break;
      case "return":
        derivation(step.outcome.values, location);
        break;
      case "complete":
        break;
    }
  }

  return { defs, uses };
}

/** Every operation's bindings, and every definition by name — ids are
 *  unique model-wide, so a bound name resolves to one operation. */
export interface ModelBindings {
  byOp: Map<Id, OperationBindings>;
  defs: Map<Id, BindingDef>;
}

export function modelBindings(model: Model): ModelBindings {
  const byOp = new Map<Id, OperationBindings>();
  const defs = new Map<Id, BindingDef>();
  for (const [opId, op] of Object.entries(model.operations)) {
    const bindings = operationBindings(opId, op);
    byOp.set(opId, bindings);
    for (const [name, def] of bindings.defs) if (!defs.has(name)) defs.set(name, def);
  }
  return { byOp, defs };
}

/** A selection on the operation page: the key of a card, and the detail
 *  to open with it. Structurally a `DetailTarget`, spelled here so this
 *  module stays free of the view state. */
export interface ProgramSelection {
  key: string;
  id: Id;
  ctx: {
    txStep?: { op: Id; tx: Id; index: number };
    step?: { op: Id; location: string };
  };
}

/** The card of a program step, as `OperationView` keys it: `tx:<id>` for
 *  a transaction, `fx:<location>:<effect>` for a direct execution or
 *  launch, `fi:<location>:<intent>` for an intent execution or launch,
 *  `step:<location>` for everything else. */
export function stepSelection(opId: Id, location: string, step: OperationStep): ProgramSelection {
  switch (step.kind) {
    case "transaction":
      return { key: `tx:${step.transaction.id}`, id: step.transaction.id, ctx: {} };
    case "execute_effect":
    case "execute_effect_async":
      return { key: `fx:${location}:${step.effect_id}`, id: step.effect_id, ctx: {} };
    case "execute_effect_intent":
    case "execute_effect_intent_async":
      return { key: `fi:${location}:${step.intent}`, id: step.intent, ctx: {} };
    default:
      return { key: `step:${location}`, id: opId, ctx: { step: { op: opId, location } } };
  }
}

/** The producing card of a binding and the detail to open: the binding
 *  transaction step for a read, output, or intent; the binding's own
 *  detail for a result or handle. */
export function producerSelection(def: BindingDef): ProgramSelection {
  switch (def.site) {
    case "transaction":
      return {
        key: `tx:${def.transaction}`,
        id: def.transaction ?? def.name,
        ctx: def.transaction !== undefined && def.txStep !== undefined
          ? { txStep: { op: def.op, tx: def.transaction, index: def.txStep } }
          : {},
      };
    case "effect":
      return { key: `fx:${def.location}:${def.effect}`, id: def.name, ctx: {} };
    case "intent":
      return { key: `fi:${def.location}:${def.intent}`, id: def.name, ctx: {} };
    case "step":
      return { key: `step:${def.location}`, id: def.name, ctx: {} };
  }
}

/** Where a binding is bound, as a reader would say it: the program step
 *  and, inside a transaction, the transaction step. */
export function definedAtLabel(def: BindingDef): string {
  return def.txStep !== undefined ? `${def.location} · tx step ${def.txStep + 1}` : def.location;
}

/** Where a binding is used, the same way. */
export function usedAtLabel(use: BindingUse): string {
  return use.txStep !== undefined ? `${use.location} · tx step ${use.txStep + 1}` : use.location;
}
