// L1 — the runtime realization, read the way the system view draws it.
//
// L0 says what application machine exists; L1 says how invocations and
// persistent data are arranged in one realization of it (§1). Nothing
// here invents a runtime fact: an absent declaration is an absent fact,
// never a default (§22), so a boundary with no router yields no
// realization and an object with no storage layout is drawn unpartitioned
// because that is all the model says, not because it is claimed so.
//
// The realization does not live in its own graph. A router or a
// subscription dispatch is a fact *about a boundary* — the path a caller
// or a topic takes into an operation — so it is drawn on that path; a
// storage layout is a fact *about an object*, so the objects are drawn
// and the layout marks them.

import type { Graph } from "../types/graph";
import type { Id, Model } from "../types/model";
import { operationTransactions } from "./index";

/** How one invocation boundary is realized: the pool that executes it,
 *  its member concurrency, and the member-affinity fact, if any. Drawn on
 *  the path into the operation — the request boundary or the subscribe
 *  edge — because that is what it is a fact about. */
export interface BoundaryLink {
  id: string;
  operation: Id;
  input: Id;
  kind: "request" | "subscription";
  pool: Id;
  /** The pool's member concurrency, as the graph spells it. */
  concurrency: string;
  /** The router declaring a request boundary's realization; a
   *  subscription's dispatch is declared inline on the input, so the
   *  input id is what its detail opens. */
  detail: Id;
  /** The semantic routing key, or null when no member-affinity fact is
   *  declared — the absence of a fact, not a routing mode. */
  routingKey: string | null;
  memberAssignment: string | null;
}

/** A persistent object an operation's transactions touch, and how it is
 *  stored. Partitioned means a storage layout is declared for it; the
 *  key is that layout's. */
export interface DataObjectFact {
  object: Id;
  dataModel: Id;
  partitioned: boolean;
  partitionKey: string | null;
  /** The operations whose transactions read, write or transition it. */
  operations: Id[];
}

export interface RuntimeFacts {
  links: BoundaryLink[];
  /** Every object any operation touches, partitioned or not — so the two
   *  can be told apart on sight. */
  dataObjects: DataObjectFact[];
  /** Every id the L1 model declares. */
  ids: Set<Id>;
  /** True when the model declares any L1 fact at all. */
  declared: boolean;
}

/** Operations whose transactions read, write, insert, delete, lock or
 *  transition each data object. */
export function objectTouchers(model: Model): Map<Id, Id[]> {
  const out = new Map<Id, Set<Id>>();
  const add = (object: Id, op: Id) => {
    const set = out.get(object);
    if (set) set.add(op);
    else out.set(object, new Set([op]));
  };
  for (const [opId, op] of Object.entries(model.operations)) {
    for (const tx of operationTransactions(op)) {
      for (const step of tx.steps) {
        switch (step.kind) {
          case "read":
          case "write":
          case "delete":
          case "lock":
            add(step.target.object, opId);
            break;
          case "insert":
            add(step.object, opId);
            break;
          case "transition":
            add(step.subject.object, opId);
            break;
        }
      }
    }
  }
  return new Map([...out].map(([k, v]) => [k, [...v]]));
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

  // Every touched object, marked with its storage layout when one is
  // declared. The object's data model is looked up so the object can be
  // opened and grouped; the partition key is the layout's.
  const modelOfObject = new Map<Id, Id>();
  for (const [dmId, dm] of Object.entries(model.data_models)) {
    for (const objId of Object.keys(dm.objects)) modelOfObject.set(objId, dmId);
  }
  const layoutOfObject = new Map<Id, string>(
    graph.runtime.storage_layouts.map((l) => [l.object, l.partition_key.join(", ")]),
  );
  const dataObjects: DataObjectFact[] = [...objectTouchers(model)]
    .map(([object, operations]) => ({
      object,
      dataModel: modelOfObject.get(object) ?? "",
      partitioned: layoutOfObject.has(object),
      partitionKey: layoutOfObject.get(object) ?? null,
      operations,
    }))
    .sort((a, b) => a.object.localeCompare(b.object));

  const ids = new Set<Id>([
    ...graph.runtime.execution_pools.map((p) => p.id),
    ...graph.runtime.routers.map((r) => r.id),
    ...graph.runtime.storage_layouts.map((s) => s.id),
  ]);

  const declared =
    ids.size > 0 ||
    Object.keys(runtime.topics ?? {}).length > 0 ||
    Object.keys(runtime.subscriptions ?? {}).length > 0;

  return { links, dataObjects, ids, declared };
}
