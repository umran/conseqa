import { useMemo } from "react";

import { shortId, truncate } from "../lib/ids";
import { hashes } from "../lib/route";
import { useApp } from "../state/AppState";
import type { Edge } from "../types/graph";
import { layoutSystem, type DataObjectBox, type RealizationBox } from "./layoutSystem";
import { LegendChip, LegendLine, SvgCanvas, sel } from "./SvgCanvas";
import { StatusChip, StatusRing } from "./status";

/** Transport ordering, said rather than spelled. */
const ORDER_TEXT: Record<string, string> = {
  none: "no order",
  global: "global order",
  within_group: "order within group",
};

function edgeShortLabel(e: Edge): string {
  switch (e.kind) {
    case "publish":
    case "request":
    case "client":
      return shortId(e.schema);
    case "subscribe":
      return e.schemas.map(shortId).join(", ");
    case "external":
      return "external";
  }
}

export function SystemView() {
  const { graph, report, selection, search, runtime, showRuntime } = useApp();
  const drawRuntime = showRuntime && runtime.declared;
  const layout = useMemo(
    () => layoutSystem(graph, { runtime: drawRuntime ? runtime : null }),
    [graph, runtime, drawRuntime],
  );
  const plane = layout.runtime;

  const q = search.trim().toLowerCase();
  const matches = (id: string) =>
    !q || id.toLowerCase().includes(q) || shortId(id).toLowerCase().includes(q);

  // What a selection keeps lit. The realization is a fact about a path
  // or an entity, so selecting it lights that and nothing more: a router
  // or a subscription lights only the path through it — its caller edges,
  // the vertex, the operation — not the operation's whole neighbourhood;
  // a pool lights every such path it runs (that is all "shared pool"
  // means); an access edge lights just its operation and object. An L0
  // selection keeps its old one-hop neighbourhood, with its realizations
  // and the objects it persists to along for the ride.
  const related = useMemo(() => {
    const set = new Set<string>();
    if (!selection) return set;
    set.add(selection);

    // The edges that route through a realization vertex, and their callers.
    const pathInto = (vertexId: string) => {
      for (const e of graph.edges) {
        if ("input" in e && `rt:${e.to}/${e.input}` === vertexId) {
          set.add(e.id);
          set.add(e.from);
        }
      }
    };

    // Router / subscription vertex: only its own path.
    const vertex = runtime.links.find((l) => l.id === selection);
    if (vertex) {
      set.add(vertex.operation);
      set.add(vertex.pool);
      pathInto(vertex.id);
      return set;
    }

    // Pool: every path it runs, and nothing else.
    if (runtime.links.some((l) => l.pool === selection)) {
      for (const link of runtime.links) {
        if (link.pool !== selection) continue;
        set.add(link.id);
        set.add(link.operation);
        pathInto(link.id);
      }
      return set;
    }

    // Access edge: just the operation and the object it wires.
    const access = plane?.access.find((a) => a.id === selection);
    if (access) {
      set.add(access.operation);
      set.add(access.object);
      return set;
    }

    // Object: the operations that touch it, by their access edges.
    if (plane?.dataObjects.some((o) => o.object === selection)) {
      for (const a of plane.access) {
        if (a.object === selection) {
          set.add(a.id);
          set.add(a.operation);
        }
      }
      return set;
    }

    // L0 selection: a service stands for its operations; light the
    // one-hop neighbourhood, then its realizations and accessed objects.
    for (const op of graph.services.find((s) => s.id === selection)?.operations ?? []) set.add(op);
    for (const e of graph.edges) {
      if (e.id === selection || set.has(e.from) || set.has(e.to)) {
        set.add(e.id);
        set.add(e.from);
        set.add(e.to);
      }
    }
    for (const link of runtime.links) {
      if (set.has(link.operation)) {
        set.add(link.id);
        set.add(link.pool);
        pathInto(link.id);
      }
    }
    for (const a of plane?.access ?? []) {
      if (set.has(a.operation)) {
        set.add(a.id);
        set.add(a.object);
      }
    }
    return set;
  }, [graph, runtime, plane, selection]);

  const isDim = (key: string) => (q && !matches(key)) || (!!selection && !related.has(key));

  const legend = (
    <>
      <LegendLine color="var(--arch-edge-publish)" label="publication" />
      <LegendLine color="var(--arch-edge-subscribe)" label="subscription" />
      {graph.edges.some((e) => e.kind === "request") && (
        <LegendLine color="var(--arch-edge-request)" label="request" />
      )}
      {graph.externals.length > 0 && <LegendLine color="var(--arch-edge-external)" label="external effect" />}
      {graph.client && <LegendLine color="var(--arch-edge-client)" label="client request" />}
      <LegendLine color="var(--arch-text-subtle)" label="declared, unexecuted" dashed />
      {graph.edges.some(
        (e) =>
          "async_executed_at" in e &&
          e.executed_at.length > 0 &&
          e.async_executed_at.length === e.executed_at.length,
      ) && <LegendLine color="var(--arch-text)" label="async launch — no completion dependency" dashed />}
      {drawRuntime && (
        <>
          <LegendChip color="var(--arch-l1)" label="L1 realization" />
          <LegendLine color="var(--arch-l1)" label="access, partition-keyed" />
          <LegendLine color="var(--arch-l1)" label="access, not keyed" dashed />
        </>
      )}
      {report && (
        <>
          <LegendChip color="var(--arch-proven)" label="proven" />
          <LegendChip color="var(--arch-disproven)" label="disproven" />
          <LegendChip color="var(--arch-unknown)" label="unknown" />
        </>
      )}
    </>
  );

  const empty =
    !graph.operations.length && !graph.topics.length ? "model declares no operations or topics" : null;

  return (
    <SvgCanvas legend={legend} empty={empty}>
      {/* Access edges behind everything, each carrying whether it keys to
          the object's partition, and each its own selection target. */}
      {plane?.access.map((access) => {
        const dimmed = selection ? !related.has(access.id) : false;
        const classes = ["arch-access", access.keyed ? "keyed" : "unkeyed"];
        if (dimmed) classes.push("dimmed");
        if (selection === access.id) classes.push("selected");
        return (
          <g
            key={access.id}
            data-sel={sel({ key: access.id, id: access.object, ctx: { access: { operation: access.operation, object: access.object } } })}
          >
            <path className={classes.join(" ")} d={access.d} markerEnd="url(#arr-l1)" />
            <path className="arch-access-hit" d={access.d} />
          </g>
        );
      })}

      {plane?.dataObjects.map((obj) => (
        <DataObject key={obj.object} obj={obj} dimmed={isDim(obj.object)} selected={selection === obj.object} />
      ))}

      {layout.services.map((box) => {
        const svc = graph.services.find((s) => s.id === box.id);
        return (
          <g key={box.id} className={`arch-service${isDim(box.id) ? " dimmed" : ""}`} data-sel={sel({ key: box.id, id: box.id })}>
            <rect className="box" x={box.x} y={box.y} width={box.w} height={box.h} rx={10} />
            <text className="label" x={box.x + 12} y={box.y + 20}>
              {truncate(shortId(box.id), 22)}
            </text>
            <text className="kind" x={box.x + box.w - 12} y={box.y + 20} textAnchor="end">
              {svc?.kind ?? ""}
            </text>
            <title>{box.id + (svc ? ` (${svc.kind})` : "")}</title>
          </g>
        );
      })}

      {layout.edges.map(({ edge: e, d, labelAt }) => {
        const unexecuted = "executed_at" in e && e.executed_at.length === 0;
        // Async only when every execution site is a launch: one
        // synchronous site is a completion dependency, and the solid
        // line is the honest summary.
        const allAsync =
          "async_executed_at" in e &&
          e.executed_at.length > 0 &&
          e.async_executed_at.length === e.executed_at.length;
        const dimmed = selection ? !related.has(e.id) : q ? !(matches(e.from) || matches(e.to)) : false;
        const classes = ["arch-edge", e.kind];
        if (unexecuted) classes.push("unexecuted");
        if (allAsync) classes.push("async");
        if (dimmed) classes.push("dimmed");
        if (selection === e.id) classes.push("selected");
        return (
          <g key={e.id} data-sel={sel({ key: e.id, id: e.id, ctx: { edge: true } })}>
            <path className={classes.join(" ")} d={d} markerEnd={`url(#arr-${e.kind})`} />
            <path className="arch-edge-hit" d={d} />
            {selection === e.id && (
              <text className="arch-edge-label" x={labelAt.x} y={labelAt.y}>
                {edgeShortLabel(e)}
              </text>
            )}
          </g>
        );
      })}

      {graph.operations.map((op) => {
        const p = layout.pos.get(op.id);
        if (!p) return null;
        const r = op.requirements;
        const badges: string[] = [];
        if (r.serialization) badges.push(`S${r.serialization}`);
        if (r.ordering) badges.push(`O${r.ordering}`);
        if (r.idempotency) badges.push(`I${r.idempotency}`);
        if (r.recoverability) badges.push(`R${r.recoverability}`);
        if (op.machines.length) badges.push("SM");
        const classes = ["arch-node", "operation"];
        if (isDim(op.id)) classes.push("dimmed");
        if (selection === op.id) classes.push("selected");
        return (
          <g key={op.id} className={classes.join(" ")} data-sel={sel({ key: op.id, id: op.id })} data-dbl={hashes.op(op.id)}>
            <StatusRing x={p.x} y={p.y} w={p.w} h={p.h} rx={8} obKey={op.id} />
            <rect className="body" x={p.x} y={p.y} width={p.w} height={p.h} rx={8} />
            <text className="title" x={p.x + 10} y={p.y + 20}>
              {truncate(shortId(op.id), 24)}
            </text>
            <text className="subtitle" x={p.x + 10} y={p.y + 36}>
              {`${op.steps} step${op.steps === 1 ? "" : "s"} · ${op.inputs} input${op.inputs === 1 ? "" : "s"}`}
            </text>
            <text className="badge-text" x={p.x + 10} y={p.y + 52}>
              {badges.join("  ")}
            </text>
            <title>{op.id + (op.description ? `\n${op.description}` : "") + "\n(double-click to open the program)"}</title>
            <StatusChip x={p.x + p.w - 6} y={p.y} obKey={op.id} />
          </g>
        );
      })}

      {/* Realization tabs last, so they read on top of the approach into
          the operation they belong to. */}
      {plane?.realizations.map((r) => (
        <Realization
          key={r.link.id}
          r={r}
          dimmed={isDim(r.link.id)}
          selected={selection === r.link.id}
          poolSelected={selection === r.link.pool}
        />
      ))}

      {graph.topics.map((t) => {
        const p = layout.pos.get(t.id);
        if (!p) return null;
        const classes = ["arch-node", "topic"];
        if (isDim(t.id)) classes.push("dimmed");
        if (selection === t.id) classes.push("selected");
        const n = t.messages.length;
        // The topic's own identity is L0: a logical channel and the
        // messages it carries. Ordering and grouping are transport facts
        // and belong to the layer that declares them.
        const transport = t.topic_scoped_transport
          ? `${t.grouping === "none" ? "ungrouped" : "keyed groups"} · ${ORDER_TEXT[t.ordering] ?? t.ordering}`
          : "per-subscription transport";
        return (
          <g key={t.id} className={classes.join(" ")} data-sel={sel({ key: t.id, id: t.id })}>
            <StatusRing x={p.x} y={p.y} w={p.w} h={p.h} rx={24} obKey={t.id} />
            <rect className="body" x={p.x} y={p.y} width={p.w} height={p.h} rx={24} />
            <text className="title" x={p.x + p.w / 2} y={p.y + (drawRuntime ? 21 : 25)} textAnchor="middle">
              {truncate(shortId(t.id), 24)}
            </text>
            <text className="subtitle" x={p.x + p.w / 2} y={p.y + (drawRuntime ? 35 : 39)} textAnchor="middle">
              {`topic · ${n} message${n === 1 ? "" : "s"}`}
            </text>
            {drawRuntime && (
              <text className="l1-mark" x={p.x + p.w / 2} y={p.y + 48} textAnchor="middle">
                {transport}
              </text>
            )}
            <title>{`${t.id}\nmessages: ${t.messages.map(shortId).join(", ")}\ntransport: ${transport}`}</title>
            <StatusChip x={p.x + p.w - 6} y={p.y} obKey={t.id} />
          </g>
        );
      })}

      {graph.externals.map((ext) => {
        const p = layout.pos.get(ext.id);
        if (!p) return null;
        const classes = ["arch-node", "external"];
        if (isDim(ext.id)) classes.push("dimmed");
        if (selection === ext.id) classes.push("selected");
        return (
          <g key={ext.id} className={classes.join(" ")} data-sel={sel({ key: ext.id, id: ext.id })}>
            <rect className="body" x={p.x} y={p.y} width={p.w} height={p.h} rx={6} />
            <text className="title" x={p.x + p.w / 2} y={p.y + 21} textAnchor="middle">
              {truncate(ext.name, 24)}
            </text>
            <text className="subtitle" x={p.x + p.w / 2} y={p.y + 36} textAnchor="middle">
              external system
            </text>
            <title>{`external: ${ext.name}\nthe modeled system ends here`}</title>
          </g>
        );
      })}

      {graph.client && (() => {
        const p = layout.pos.get(graph.client.id)!;
        const classes = ["arch-node", "client"];
        if (isDim(graph.client.id)) classes.push("dimmed");
        if (selection === graph.client.id) classes.push("selected");
        return (
          <g className={classes.join(" ")} data-sel={sel({ key: graph.client.id, id: graph.client.id })}>
            <rect className="body" x={p.x} y={p.y} width={p.w} height={p.h} rx={10} />
            <text className="title" x={p.x + p.w / 2} y={p.y + 24} textAnchor="middle">
              clients
            </text>
            <text className="subtitle" x={p.x + p.w / 2} y={p.y + 40} textAnchor="middle">
              unmodeled callers
            </text>
            <title>request inputs no modeled operation invokes</title>
          </g>
        );
      })()}
    </SvgCanvas>
  );
}

/** One boundary's realization, on the approach into the operation it
 *  realizes. The pool name is its own click target: selecting a pool
 *  lights every tab that names it. */
function Realization({
  r,
  dimmed,
  selected,
  poolSelected,
}: {
  r: RealizationBox;
  dimmed: boolean;
  selected: boolean;
  poolSelected: boolean;
}) {
  const { link } = r;
  const classes = ["arch-real", link.kind];
  if (dimmed) classes.push("dimmed");
  if (selected || poolSelected) classes.push("selected");
  const affinity = link.routingKey ? `keyed · ${concurrencyShort(link.concurrency)}` : concurrencyShort(link.concurrency);
  const title =
    `${link.kind === "request" ? "request boundary" : "subscription"} of ${link.operation} · ${link.input}\n` +
    `pool ${link.pool} — ${link.concurrency} per member\n` +
    (link.routingKey ? `routed by ${link.routingKey} (${link.memberAssignment})` : "no member-affinity fact declared");
  return (
    <g className={classes.join(" ")}>
      <path className="arch-real-arm" d={r.connector} />
      <g data-sel={sel({ key: link.id, id: link.detail })}>
        <rect className="body" x={r.x} y={r.y} width={r.w} height={r.h} rx={7} />
        <text className="kind-mark" x={r.x + 8} y={r.y + 14}>
          {link.kind === "request" ? "▸ request" : "◃ subscribe"}
        </text>
        <text className="affinity" x={r.x + r.w - 8} y={r.y + 14} textAnchor="end">
          {affinity}
        </text>
        <title>{title}</title>
      </g>
      {/* The pool is a separate target, so a reader can pivot to
          everything that shares it. */}
      <text
        className="pool"
        x={r.x + 8}
        y={r.y + 27}
        data-sel={sel({ key: link.pool, id: link.pool })}
      >
        {truncate(shortId(link.pool), 22)}
      </text>
    </g>
  );
}

/** Concurrency, compressed for a tab: "bounded(1)" → "1/mbr". */
function concurrencyShort(concurrency: string): string {
  const m = concurrency.match(/^bounded\((\d+)\)$/);
  if (m) return `${m[1]}/mbr`;
  if (concurrency === "unbounded") return "∞/mbr";
  return "?/mbr";
}

/** A persistent object, drawn so a partitioned store is distinct on
 *  sight from one with no declared layout. */
function DataObject({ obj, dimmed, selected }: { obj: DataObjectBox; dimmed: boolean; selected: boolean }) {
  const classes = ["arch-object", obj.partitioned ? "partitioned" : "plain"];
  if (dimmed) classes.push("dimmed");
  if (selected) classes.push("selected");
  // Partitioning is carried by the node's style — spined and solid, or
  // open and dashed — and by the access edges, not by a caption.
  const title = obj.partitioned
    ? `${obj.object}\npartitioned`
    : `${obj.object}\nno storage layout declared`;
  return (
    <g className={classes.join(" ")} data-sel={sel({ key: obj.object, id: obj.object })}>
      <rect className="body" x={obj.x} y={obj.y} width={obj.w} height={obj.h} rx={8} />
      {obj.partitioned && <rect className="spine" x={obj.x + 5} y={obj.y + 6} width={3} height={obj.h - 12} rx={1.5} />}
      <text className="title" x={obj.x + 16} y={obj.y + 26}>
        {truncate(shortId(obj.object), 20)}
      </text>
      <text className="subtitle" x={obj.x + 16} y={obj.y + 42}>
        {obj.dataModel ? shortId(obj.dataModel) : "data object"}
      </text>
      <title>{title}</title>
    </g>
  );
}
