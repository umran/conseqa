// L1 — the runtime realization, read the way the system view draws it.
//
// L0 says what application machine exists; L1 says how invocations and
// persistent data are arranged in one realization of it (§1). Nothing
// here invents a runtime fact: an absent declaration is an absent fact,
// never a default (§22), so a boundary with no router yields no link and
// a subscription with no declared runtime yields no dispatch.

import type { Graph } from "../types/graph";
import type { Id, Model } from "../types/model";
import { operationTransactions } from "./index";

/** One invocation boundary's realization: which pool executes it, and on
 *  what member-affinity fact, if any. */
export interface BoundaryLink {
  id: string;
  operation: Id;
  input: Id;
  kind: "request" | "subscription";
  pool: Id;
  /** The router declaring this request boundary's realization. Subscription
   *  dispatch is declared inline on the subscription and has no id. */
  router: Id | null;
  /** The semantic routing key, or null when no member-affinity fact is
   *  declared — which is the absence of a fact, not a routing mode. */
  routingKey: string | null;
  memberAssignment: string | null;
}

export interface RuntimeFacts {
  links: BoundaryLink[];
  /** Data objects a storage layout partitions, and the operations whose
   *  transactions touch them. */
  storage: { id: Id; dataModel: Id; object: Id; partitionKey: string; operations: Id[] }[];
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
  const links: BoundaryLink[] = [];

  for (const router of graph.runtime.routers) {
    links.push({
      id: `rt:${router.operation}/${router.input}`,
      operation: router.operation,
      input: router.input,
      kind: "request",
      pool: router.pool,
      router: router.id,
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
        router: null,
        routingKey: sub.dispatch.routing ? "grouping key" : null,
        memberAssignment: sub.dispatch.routing?.member_assignment.kind.replace("_", "-") ?? null,
      });
    }
  }

  const touchers = objectTouchers(model);
  const storage = graph.runtime.storage_layouts.map((layout) => ({
    id: layout.id,
    dataModel: layout.data_model,
    object: layout.object,
    partitionKey: layout.partition_key.join(", "),
    operations: touchers.get(layout.object) ?? [],
  }));

  const ids = new Set<Id>([
    ...graph.runtime.execution_pools.map((p) => p.id),
    ...graph.runtime.routers.map((r) => r.id),
    ...graph.runtime.storage_layouts.map((s) => s.id),
  ]);

  const declared =
    ids.size > 0 ||
    Object.keys(runtime.topics ?? {}).length > 0 ||
    Object.keys(runtime.subscriptions ?? {}).length > 0;

  return { links, storage, ids, declared };
}
