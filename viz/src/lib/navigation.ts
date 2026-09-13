// Where things are: the topological hierarchy the pages hang off, read
// from the model.
//
// The main canvas shows one thing at a time, and everything it can show
// has a place in one tree: the system holds services, topics, data
// models, state machines, schemas, the runtime realization's
// declarations, and the boundary the model stops at; a service holds its
// operations; an operation its inline transactions; a data model its
// objects and outboxes. The breadcrumbs are the path down that tree to
// the page in view, and the navigator is the tree itself. The runtime
// overview is the one page with no node of its own: it is the `runtime`
// step of every L1 declaration's breadcrumbs.

import { CLIENT_NODE_ID, EXTERNAL_PREFIX, type Graph } from "../types/graph";
import type { Model } from "../types/model";
import { shortId } from "./ids";
import { operationTransactions, type ModelIndex } from "./index";
import { hashes, type Route } from "./route";

/** What a page in the main canvas can be about. */
export type PageKind =
  | "system" | "runtime"
  | "service" | "operation" | "transaction" | "machine"
  | "topic" | "outbox" | "data_model" | "object" | "schema"
  | "pool" | "router" | "storage_layout"
  | "external" | "clients";

/** One step of the path from the system to a page. */
export interface Crumb {
  kind: PageKind;
  id: string;
  label: string;
  hash: string;
}

/** The kind of page an id opens, or null for an id that has no page of
 *  its own — a step, a binding, an input, an effect, a state, a
 *  transition — and is shown in the inspector on its owner's page. */
export function pageKindOf(id: string, index: ModelIndex): PageKind | null {
  if (id === CLIENT_NODE_ID) return "clients";
  if (id.startsWith(EXTERNAL_PREFIX)) return "external";
  const entry = index.get(id);
  if (!entry) return null;
  switch (entry.kind) {
    case "service": return "service";
    case "operation": return "operation";
    case "transaction": return "transaction";
    case "machine": return "machine";
    case "topic": return "topic";
    case "outbox": return "outbox";
    case "data_model": return "data_model";
    case "object": return "object";
    case "schema": return "schema";
    case "pool": return "pool";
    case "router": return "router";
    case "storage_layout": return "storage_layout";
    default: return null;
  }
}

/** The hash of an entity's page, or null when it has none. */
export function pageHash(id: string, index: ModelIndex): string | null {
  const kind = pageKindOf(id, index);
  if (!kind) return null;
  return hashFor(kind, id);
}

function hashFor(kind: PageKind, id: string): string {
  switch (kind) {
    case "system": return hashes.system();
    case "runtime": return hashes.runtime();
    case "service": return hashes.entity("service", id);
    case "operation": return hashes.op(id);
    case "transaction": return hashes.tx(id);
    case "machine": return hashes.machine(id);
    case "topic": return hashes.entity("topic", id);
    case "outbox": return hashes.entity("outbox", id);
    case "data_model": return hashes.entity("data", id);
    case "object": return hashes.entity("object", id);
    case "schema": return hashes.entity("schema", id);
    case "pool": return hashes.entity("pool", id);
    case "router": return hashes.entity("router", id);
    case "storage_layout": return hashes.entity("storage", id);
    case "external": return hashes.external(id.slice(EXTERNAL_PREFIX.length));
    case "clients": return hashes.clients();
  }
}

/** A page's label: the id without its conventional kind prefix. */
export function pageLabel(kind: PageKind, id: string): string {
  switch (kind) {
    case "system": return "system";
    case "runtime": return "runtime";
    case "clients": return "clients";
    case "external": return id.slice(EXTERNAL_PREFIX.length);
    default: return shortId(id);
  }
}

/** The kind of a page, said for a caption. */
export const PAGE_KIND_LABEL: Record<PageKind, string> = {
  system: "system",
  runtime: "runtime · L1",
  service: "service",
  operation: "operation",
  transaction: "transaction",
  machine: "state machine",
  topic: "topic",
  outbox: "outbox",
  data_model: "data model",
  object: "data object",
  schema: "schema",
  pool: "execution pool",
  router: "router",
  storage_layout: "storage layout",
  external: "external system",
  clients: "clients",
};

function crumb(kind: PageKind, id: string): Crumb {
  return { kind, id, label: pageLabel(kind, id), hash: hashFor(kind, id) };
}

/** The path from the system to the page a route shows, the page last.
 *  A transaction sits under the operation whose program declares it, an
 *  object or an outbox under its data model, and every L1 declaration
 *  under the runtime realization; a state machine sits directly under
 *  the system, as it does in the navigator. */
export function ancestry(route: Route, model: Model, index: ModelIndex): Crumb[] {
  const root = crumb("system", "");
  switch (route.view) {
    case "system":
      return [root];
    case "runtime":
      return [root, crumb("runtime", "")];
    case "op": {
      const op = model.operations[route.id];
      return op
        ? [root, crumb("service", op.service), crumb("operation", route.id)]
        : [root, crumb("operation", route.id)];
    }
    case "tx": {
      const entry = index.get(route.id);
      const opId = entry?.kind === "transaction" ? entry.op : null;
      const op = opId ? model.operations[opId] : null;
      return op && opId
        ? [root, crumb("service", op.service), crumb("operation", opId), crumb("transaction", route.id)]
        : [root, crumb("transaction", route.id)];
    }
    case "machine":
      return [root, crumb("machine", route.id)];
    case "entity": {
      const kind = pageKindOf(route.id, index);
      if (!kind) return [root, { kind: "system", id: route.id, label: route.id, hash: hashes.system() }];
      switch (kind) {
        case "outbox":
        case "object": {
          const entry = index.get(route.id);
          const dm = entry && "dataModel" in entry ? entry.dataModel : null;
          return dm ? [root, crumb("data_model", dm), crumb(kind, route.id)] : [root, crumb(kind, route.id)];
        }
        case "pool":
        case "router":
        case "storage_layout":
          return [root, crumb("runtime", ""), crumb(kind, route.id)];
        default:
          return [root, crumb(kind, route.id)];
      }
    }
  }
}

// ---------------------------------------------------------------------
// The navigator's tree
// ---------------------------------------------------------------------

export interface NavNode {
  id: string;
  kind: PageKind;
  label: string;
  hash: string;
  /** The keys the obligation index anchors this entity's verdicts under:
   *  one for most entities, and for a state machine its own and each of
   *  its transitions', which have no node of their own. */
  obKeys: string[];
  children: NavNode[];
}

export interface NavGroup {
  key: string;
  title: string;
  nodes: NavNode[];
  /** Whether the group starts expanded. */
  open: boolean;
}

function node(kind: PageKind, id: string, extra: Partial<NavNode> = {}): NavNode {
  return { id, kind, label: pageLabel(kind, id), hash: hashFor(kind, id), obKeys: [], children: [], ...extra };
}

const byId = (a: { id: string }, b: { id: string }) => a.id.localeCompare(b.id);

/** The model as a tree of pages. */
export function navigationTree(model: Model, graph: Graph): NavGroup[] {
  const groups: NavGroup[] = [];

  const services = [...graph.services].sort(byId).map((svc) =>
    node("service", svc.id, {
      children: [...svc.operations].sort().map((opId) => {
        const op = model.operations[opId];
        const txs = op ? operationTransactions(op) : [];
        return node("operation", opId, {
          obKeys: [opId],
          children: txs.map((tx) => node("transaction", tx.id, { obKeys: [`${opId}/${tx.id}`] })),
        });
      }),
    }),
  );
  groups.push({ key: "services", title: "services", nodes: services, open: true });

  const topics = [...graph.topics].sort(byId).map((t) => node("topic", t.id, { obKeys: [t.id] }));
  if (topics.length) groups.push({ key: "topics", title: "topics", nodes: topics, open: true });

  const data = Object.entries(model.data_models)
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([dmId, dm]) =>
      node("data_model", dmId, {
        children: [
          ...Object.keys(dm.objects).sort().map((objId) => node("object", objId, { obKeys: [`${dmId}/${objId}`] })),
          ...Object.keys(dm.outboxes ?? {}).sort().map((obId) => node("outbox", obId)),
        ],
      }),
    );
  if (data.length) groups.push({ key: "data", title: "data", nodes: data, open: true });

  // A machine's page is where its transitions are read, so a verdict on
  // any transition rolls up into the machine's dot.
  const machines = Object.entries(model.state_machines)
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([mId, m]) =>
      node("machine", mId, { obKeys: [mId, ...Object.keys(m.transitions).map((t) => `${mId}/${t}`)] }),
    );
  if (machines.length) groups.push({ key: "machines", title: "state machines", nodes: machines, open: true });

  const schemas = Object.keys(model.schemas).sort().map((id) => node("schema", id));
  if (schemas.length) groups.push({ key: "schemas", title: "schemas", nodes: schemas, open: false });

  // The declarations themselves, one level: pools, then routers, then
  // storage layouts, each captioned with its kind.
  const runtime: NavNode[] = [
    ...[...graph.runtime.execution_pools].sort(byId).map((p) => node("pool", p.id)),
    ...[...graph.runtime.routers].sort(byId).map((r) => node("router", r.id)),
    ...[...graph.runtime.storage_layouts].sort(byId).map((s) => node("storage_layout", s.id)),
  ];
  if (runtime.length) groups.push({ key: "runtime", title: "runtime · L1", nodes: runtime, open: false });

  const boundary: NavNode[] = [];
  if (graph.client) boundary.push(node("clients", CLIENT_NODE_ID));
  for (const ext of [...graph.externals].sort(byId)) boundary.push(node("external", ext.id));
  if (boundary.length) groups.push({ key: "boundary", title: "boundary", nodes: boundary, open: false });

  return groups;
}

/** The ids on the path to a route's page, the page included — what the
 *  navigator expands and marks. */
export function routePath(route: Route, model: Model, index: ModelIndex): string[] {
  return ancestry(route, model, index).map((c) => pathId(c.kind, c.id));
}

/** A node's identity in the navigator: the entity id, or the kind for
 *  the two pages that are about the whole model. */
export function pathId(kind: PageKind, id: string): string {
  return kind === "system" || kind === "runtime" ? `@${kind}` : id;
}
