import { useSyncExternalStore } from "react";

// The address bar names what the main canvas shows. Every page-bearing
// entity of the model — the system, a service, an operation, one of its
// transactions, a state machine, a topic, an outbox, a data model, an
// object, a schema, the runtime realization and each of its
// declarations, the clients, an external system — has a page and so a
// hash of its own, so a deep link, the browser history, and the
// breadcrumbs all agree on where the reader is.

/** The transaction families a transaction page can be opened on. */
export type TransactionRequirementKind = "transaction_serializability" | "transaction_ordering";

/** One declared requirement of a transaction, by family and index. */
export interface RequirementRef {
  prop: TransactionRequirementKind;
  index: number;
}

export type Route =
  | { view: "system" }
  | { view: "runtime" }
  | { view: "op"; id: string }
  | { view: "machine"; id: string; highlight: string | null }
  /** A transaction's page, optionally opened on one of its requirements. */
  | { view: "tx"; id: string; req: RequirementRef | null }
  /** Any other page-bearing entity, by id; the page dispatches on what
   *  the id names. `@client` and `@external:<name>` are the synthetic
   *  boundary vertices of the system graph. */
  | { view: "entity"; id: string };

/** The URL segment each entity kind is addressed under. Cosmetic — the
 *  id decides what the page shows — but a hash that reads
 *  `#/topic/topic.orders` says what it opens. */
export type EntitySegment =
  | "service" | "topic" | "outbox" | "data" | "object" | "schema" | "pool" | "router" | "storage";

const ENTITY_SEGMENTS = new Set<string>([
  "service", "topic", "outbox", "data", "object", "schema", "pool", "router", "storage",
]);

const REQ_SEGMENT: Record<TransactionRequirementKind, string> = {
  transaction_serializability: "serializability",
  transaction_ordering: "ordering",
};

function parseReq(text: string | undefined): RequirementRef | null {
  if (!text) return null;
  const m = text.match(/^(serializability|ordering)\.(\d+)$/);
  if (!m) return null;
  return {
    prop: m[1] === "serializability" ? "transaction_serializability" : "transaction_ordering",
    index: Number(m[2]),
  };
}

export function parseHash(hash: string): Route {
  const h = decodeURIComponent(hash || "");
  let m: RegExpMatchArray | null;
  if (h === "#/runtime") return { view: "runtime" };
  if (h === "#/clients") return { view: "entity", id: "@client" };
  // An old `?flow=` query is tolerated and ignored: the operation page
  // shows its one program.
  if ((m = h.match(/^#\/op\/([^?]+)(?:\?.*)?$/))) {
    return { view: "op", id: m[1] };
  }
  if ((m = h.match(/^#\/machine\/([^?]+)(?:\?t=(.+))?$/))) {
    return { view: "machine", id: m[1], highlight: m[2] ?? null };
  }
  if ((m = h.match(/^#\/tx\/([^?]+)(?:\?req=(.+))?$/))) {
    return { view: "tx", id: m[1], req: parseReq(m[2]) };
  }
  if ((m = h.match(/^#\/external\/(.+)$/))) {
    return { view: "entity", id: `@external:${m[1]}` };
  }
  if ((m = h.match(/^#\/([a-z]+)\/([^?]+)$/)) && ENTITY_SEGMENTS.has(m[1])) {
    return { view: "entity", id: m[2] };
  }
  return { view: "system" };
}

export function routeKey(route: Route): string {
  switch (route.view) {
    case "system": return "system";
    case "runtime": return "runtime";
    case "op": return `op:${route.id}`;
    case "machine": return `machine:${route.id}` + (route.highlight ? `?${route.highlight}` : "");
    case "tx": return `tx:${route.id}` + (route.req ? `?${REQ_SEGMENT[route.req.prop]}.${route.req.index}` : "");
    case "entity": return `entity:${route.id}`;
  }
}

/** The subject a route names by itself: a machine route's transition.
 *  The page selects it, so deep links and history navigation agree with
 *  what the address bar says. */
export function impliedSubject(route: Route): string | null {
  return route.view === "machine" ? route.highlight : null;
}

/** The entity a page is about, when it is about one: the operation,
 *  the machine, the transaction, or the entity the route names. The
 *  system and runtime overviews are about the whole model. */
export function routeSubject(route: Route): string | null {
  switch (route.view) {
    case "op":
    case "machine":
    case "tx":
    case "entity":
      return route.id;
    default:
      return null;
  }
}

export const hashes = {
  system: () => "#/system",
  runtime: () => "#/runtime",
  op: (id: string) => `#/op/${encodeURIComponent(id)}`,
  machine: (id: string, transition?: string) =>
    `#/machine/${encodeURIComponent(id)}` +
    (transition ? `?t=${encodeURIComponent(transition)}` : ""),
  tx: (id: string, req?: RequirementRef | null) =>
    `#/tx/${encodeURIComponent(id)}` + (req ? `?req=${REQ_SEGMENT[req.prop]}.${req.index}` : ""),
  entity: (segment: EntitySegment, id: string) => `#/${segment}/${encodeURIComponent(id)}`,
  clients: () => "#/clients",
  external: (name: string) => `#/external/${encodeURIComponent(name)}`,
};

const listeners = new Set<() => void>();

function subscribe(listener: () => void) {
  listeners.add(listener);
  window.addEventListener("hashchange", listener);
  return () => {
    listeners.delete(listener);
    window.removeEventListener("hashchange", listener);
  };
}

function snapshot() {
  return window.location.hash;
}

export function useRoute(): Route {
  const hash = useSyncExternalStore(subscribe, snapshot);
  return parseHash(hash);
}

export function navigate(hash: string) {
  if (window.location.hash === hash) {
    for (const listener of listeners) listener();
  } else {
    window.location.hash = hash;
  }
}
