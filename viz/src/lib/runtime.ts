// L1 — the runtime realization, read the way the system view draws it.
//
// L0 says what application machine exists; L1 says how invocations and
// persistent data are arranged in one realization of it (§1). Nothing
// here invents a runtime fact: an absent declaration is an absent fact,
// never a default (§22), so a boundary with no router yields no
// realization, and an object with no storage layout is drawn
// unpartitioned because that is all the model says.
//
// The realization does not live in its own graph. A router or a
// subscription dispatch is a fact about a boundary — the path a caller
// or a topic takes into an operation — so it sits on that path as an
// intermediate vertex; a storage layout is a fact about an object, so
// the objects are drawn and the access edges say whether each access
// keys to the partition.

import type { Graph } from "../types/graph";
import type { FieldPath, Id, Model, SelectorPredicate } from "../types/model";
import { operationTransactions } from "./index";

/** How one invocation boundary is realized: the pool that executes it,
 *  its member concurrency, and the member-affinity fact, if any. Drawn
 *  as a vertex on the path into the operation — the request boundary or
 *  the subscribe edge — because that is what it is a fact about. */
export interface BoundaryLink {
  id: string;
  operation: Id;
  input: Id;
  kind: "request" | "subscription";
  pool: Id;
  concurrency: string;
  /** What the vertex opens: a request boundary's router, or, for a
   *  subscription, the input its dispatch is declared on. */
  detail: Id;
  routingKey: string | null;
  memberAssignment: string | null;
}

/** A persistent object an operation's transactions touch, and how it is
 *  stored. Partitioned means a storage layout is declared for it. */
export interface DataObjectFact {
  object: Id;
  dataModel: Id;
  partitioned: boolean;
  operations: Id[];
}

/** One operation's access to one object. `keyed` is the fact the edge
 *  carries: whether the access confines itself to a partition — every
 *  selector pins the partition key (an insert keys by its values). An
 *  access to an unpartitioned object is never keyed: there is no
 *  partition to key to. */
export interface AccessFact {
  id: string;
  operation: Id;
  object: Id;
  partitioned: boolean;
  keyed: boolean;
}

export interface RuntimeFacts {
  links: BoundaryLink[];
  dataObjects: DataObjectFact[];
  access: AccessFact[];
  ids: Set<Id>;
  declared: boolean;
}

function samePath(a: FieldPath, b: FieldPath): boolean {
  return a.length === b.length && a.every((s, i) => s === b[i]);
}

/** Every field path a predicate pins by equality, gathered through `and`. */
function pinnedFields(p: SelectorPredicate): FieldPath[] {
  if (p.kind === "eq") return [p.field];
  if (p.kind === "and") return p.predicates.flatMap(pinnedFields);
  return [];
}

/** Whether a selector confines to a partition: it pins every key field. */
function pinsKey(predicate: SelectorPredicate, key: FieldPath[]): boolean {
  const pinned = pinnedFields(predicate);
  return key.every((k) => pinned.some((f) => samePath(f, k)));
}

/** Every operation's selector predicates against each object, by the
 *  transaction steps that bear one. Insert has no selector — it writes a
 *  row whose values place it — so it is recorded with a null predicate. */
export function objectAccesses(model: Model): Map<Id, Map<Id, (SelectorPredicate | null)[]>> {
  const out = new Map<Id, Map<Id, (SelectorPredicate | null)[]>>();
  const add = (object: Id, op: Id, predicate: SelectorPredicate | null) => {
    let byOp = out.get(object);
    if (!byOp) out.set(object, (byOp = new Map()));
    const list = byOp.get(op);
    if (list) list.push(predicate);
    else byOp.set(op, [predicate]);
  };
  for (const [opId, op] of Object.entries(model.operations)) {
    for (const tx of operationTransactions(op)) {
      for (const step of tx.steps) {
        switch (step.kind) {
          case "read":
          case "write":
          case "delete":
          case "lock":
            add(step.target.object, opId, step.target.predicate);
            break;
          case "insert":
            add(step.object, opId, null);
            break;
          case "transition":
            add(step.subject.object, opId, step.subject.predicate);
            break;
        }
      }
    }
  }
  return out;
}

/** Whether an operation's whole access to an object keys to the given
 *  partition key: every selector-bearing step pins it (inserts, having
 *  no selector, key by their values and do not break this). */
export function accessKeysToPartition(
  predicates: (SelectorPredicate | null)[],
  key: FieldPath[],
): boolean {
  return predicates.every((p) => p === null || pinsKey(p, key));
}

/** The partition key declared for an object, or null when none is. */
export function partitionKeyOf(model: Model, object: Id): FieldPath[] | null {
  for (const layout of Object.values(model.runtime?.storage_layouts ?? {})) {
    if (layout.object.object === object) return layout.partition_key;
  }
  return null;
}

export function runtimeFacts(model: Model, graph: Graph): RuntimeFacts {
  const runtime = model.runtime ?? {};
  const concurrencyOf = new Map(
    graph.runtime.execution_pools.map((p) => [p.id, p.member_concurrency]),
  );

  const links: BoundaryLink[] = [];
  for (const router of graph.runtime.routers) {
    links.push({
      id: `rt:${router.operation}/${router.input}`,
      operation: router.operation,
      input: router.input,
      kind: "request",
      pool: router.pool,
      concurrency: concurrencyOf.get(router.pool) ?? "unspecified",
      detail: router.id,
      routingKey: router.routing_key.length ? router.routing_key.join(", ") : null,
      memberAssignment: router.member_assignment,
    });
  }
  for (const [opId, inputs] of Object.entries(runtime.subscriptions ?? {})) {
    for (const [inputId, sub] of Object.entries(inputs)) {
      links.push({
        id: `rt:${opId}/${inputId}`,
        operation: opId,
        input: inputId,
        kind: "subscription",
        pool: sub.dispatch.pool,
        concurrency: concurrencyOf.get(sub.dispatch.pool) ?? "unspecified",
        detail: inputId,
        routingKey: sub.dispatch.routing ? "grouping key" : null,
        memberAssignment: sub.dispatch.routing?.member_assignment.kind.replace("_", "-") ?? null,
      });
    }
  }

  const modelOfObject = new Map<Id, Id>();
  for (const [dmId, dm] of Object.entries(model.data_models)) {
    for (const objId of Object.keys(dm.objects)) modelOfObject.set(objId, dmId);
  }

  const accesses = objectAccesses(model);
  const dataObjects: DataObjectFact[] = [];
  const access: AccessFact[] = [];
  for (const [object, byOp] of accesses) {
    const key = partitionKeyOf(model, object);
    dataObjects.push({
      object,
      dataModel: modelOfObject.get(object) ?? "",
      partitioned: key !== null,
      operations: [...byOp.keys()].sort(),
    });
    for (const [op, predicates] of byOp) {
      access.push({
        id: `ax:${op}:${object}`,
        operation: op,
        object,
        partitioned: key !== null,
        keyed: key !== null && accessKeysToPartition(predicates, key),
      });
    }
  }
  dataObjects.sort((a, b) => a.object.localeCompare(b.object));

  const ids = new Set<Id>([
    ...graph.runtime.execution_pools.map((p) => p.id),
    ...graph.runtime.routers.map((r) => r.id),
    ...graph.runtime.storage_layouts.map((s) => s.id),
  ]);

  const declared =
    ids.size > 0 ||
    Object.keys(runtime.topics ?? {}).length > 0 ||
    Object.keys(runtime.subscriptions ?? {}).length > 0;

  return { links, dataObjects, access, ids, declared };
}
