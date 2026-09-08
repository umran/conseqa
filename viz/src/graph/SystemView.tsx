import { useMemo } from "react";

import { shortId, truncate } from "../lib/ids";
import { hashes } from "../lib/route";
import { useApp } from "../state/AppState";
import type { Edge } from "../types/graph";
import { layoutSystem, type Box, type PoolBox, type StorageBox, type SystemLayout } from "./layoutSystem";
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

  // What a selection keeps lit. Across the layer boundary too: selecting
  // a pool lights the operations it runs, and selecting an operation
  // lights the pool that runs it — the relation the band states in words
  // is the one a reader wants to see.
  const related = useMemo(() => {
    const set = new Set<string>();
    if (!selection) return set;
    set.add(selection);
    // A service stands for its operations: selecting the boundary keeps
    // everything inside it, and everything it touches, lit.
    for (const op of graph.services.find((s) => s.id === selection)?.operations ?? []) set.add(op);
    for (const e of graph.edges) {
      if (e.id === selection || set.has(e.from) || set.has(e.to)) {
        set.add(e.id);
        set.add(e.from);
        set.add(e.to);
      }
    }
    for (const link of runtime.links) {
      if (set.has(link.operation) || selection === link.pool || selection === link.id) {
        set.add(link.id);
        set.add(link.pool);
        set.add(link.operation);
      }
    }
    for (const layoutNode of runtime.storage) {
      if (selection === layoutNode.id) for (const op of layoutNode.operations) set.add(op);
      if (layoutNode.operations.includes(selection)) set.add(layoutNode.id);
    }
    return set;
  }, [graph, runtime, selection]);

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
      {drawRuntime && <LegendLine color="var(--arch-l1)" label="L1 realization" dashed />}
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
      {plane?.band && (
        <RuntimeBand layout={layout} plane={plane} band={plane.band} dim={isDim} selection={selection} />
      )}

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
        const dimmed = selection ? !related.has(e.id) : q ? !(matches(e.from) || matches(e.to)) : false;
        const classes = ["arch-edge", e.kind];
        if (unexecuted) classes.push("unexecuted");
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

      {/* Realization links, only for what is selected: the relation is
          named in the band at all times, and drawn when it is asked for. */}
      {plane?.links.map(({ id, link, d, labelAt }) => {
        if (!selection || !related.has(id)) return null;
        const label = link.routingKey ? `routed by ${link.routingKey}` : "no member affinity";
        return (
          <g key={id}>
            <path className="arch-l1-link" d={d} markerEnd="url(#arr-l1)" />
            <text className="arch-l1-label" x={labelAt.x} y={labelAt.y} textAnchor="middle">
              {label}
            </text>
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
        const pools = drawRuntime
          ? [...new Set(runtime.links.filter((l) => l.operation === op.id).map((l) => l.pool))]
          : [];
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
            {pools.length > 0 && (
              <text className="l1-mark" x={p.x + p.w - 10} y={p.y + 36} textAnchor="end">
                {truncate(pools.map(shortId).join(" · "), 18)}
              </text>
            )}
            <title>{op.id + (op.description ? `\n${op.description}` : "") + "\n(double-click to open the program)"}</title>
            <StatusChip x={p.x + p.w - 6} y={p.y} obKey={op.id} />
          </g>
        );
      })}

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

/** The L1 plane, drawn beneath the machine it realizes. */
function RuntimeBand({
  layout,
  plane,
  band,
  dim,
  selection,
}: {
  layout: SystemLayout;
  plane: NonNullable<SystemLayout["runtime"]>;
  band: Box;
  dim: (id: string) => boolean;
  selection: string | null;
}) {
  const left = Math.min(layout.l0.x, band.x) - 40;
  const right = Math.max(layout.l0.x + layout.l0.w, band.x + band.w) + 40;
  const dividerY = (layout.l0.y + layout.l0.h + band.y) / 2;
  return (
    <g className="arch-plane">
      <rect
        className="band"
        x={left}
        y={dividerY + 10}
        width={right - left}
        height={band.y + band.h - dividerY + 14}
        rx={18}
      />
      <line className="divider" x1={left} y1={dividerY} x2={right} y2={dividerY} />
      <text className="plane-label above" x={left + 4} y={dividerY - 10}>
        L0 · application machine
      </text>
      <text className="plane-label below" x={left + 4} y={dividerY + 26}>
        L1 · runtime realization — one way this machine is run
      </text>

      {plane.pools.map((pool) => (
        <Pool key={pool.id} pool={pool} dimmed={dim(pool.id)} selected={selection === pool.id} />
      ))}
      {plane.storage.map((store) => (
        <Storage key={store.id} store={store} dimmed={dim(store.id)} selected={selection === store.id} />
      ))}
    </g>
  );
}

function Pool({ pool, dimmed, selected }: { pool: PoolBox; dimmed: boolean; selected: boolean }) {
  const classes = ["arch-l1-node", "pool"];
  if (dimmed) classes.push("dimmed");
  if (selected) classes.push("selected");
  return (
    <g className={classes.join(" ")}>
      <g data-sel={sel({ key: pool.id, id: pool.id })}>
        <rect className="body" x={pool.x} y={pool.y} width={pool.w} height={pool.h} rx={12} />
        <text className="title" x={pool.x + 12} y={pool.y + 20}>
          {truncate(shortId(pool.id), 26)}
        </text>
        <text className="subtitle" x={pool.x + 12} y={pool.y + 34}>
          {`execution pool · ${pool.concurrency} per member`}
        </text>
        <title>{`${pool.id}\nmember concurrency: ${pool.concurrency}\nruns ${pool.chips.length} boundar${pool.chips.length === 1 ? "y" : "ies"}`}</title>
      </g>
      {pool.chips.map((chip) => (
        <g key={chip.id} className="chip" data-sel={sel({ key: chip.input, id: chip.input })}>
          <rect
            className={`chip-body ${chip.kind}`}
            x={chip.x}
            y={chip.y}
            width={chip.w}
            height={chip.h}
            rx={6}
          />
          <text className="chip-text" x={chip.x + 8} y={chip.y + 19}>
            {truncate(shortId(chip.operation), 20)}
          </text>
          <text className="chip-note" x={chip.x + chip.w - 8} y={chip.y + 19} textAnchor="end">
            {chip.routing ? "keyed" : chip.kind === "request" ? "request" : "subscription"}
          </text>
          <title>{`${chip.operation} · ${chip.input}\n${chip.routing ? `routed by ${chip.routing}` : "no member-affinity fact declared"}`}</title>
        </g>
      ))}
    </g>
  );
}

function Storage({ store, dimmed, selected }: { store: StorageBox; dimmed: boolean; selected: boolean }) {
  const classes = ["arch-l1-node", "storage"];
  if (dimmed) classes.push("dimmed");
  if (selected) classes.push("selected");
  return (
    <g className={classes.join(" ")} data-sel={sel({ key: store.id, id: store.id })}>
      <rect className="body" x={store.x} y={store.y} width={store.w} height={store.h} rx={8} />
      <text className="title" x={store.x + 12} y={store.y + 20}>
        {truncate(shortId(store.object), 24)}
      </text>
      <text className="subtitle" x={store.x + 12} y={store.y + 35}>
        {`partitioned by ${store.partitionKey}`}
      </text>
      <text className="l1-mark" x={store.x + 12} y={store.y + 50}>
        {`storage layout · ${store.operations.length} operation${store.operations.length === 1 ? "" : "s"}`}
      </text>
      <title>{`${store.id}\nobject ${store.object}\npartition key ${store.partitionKey}\nA partition key is not an object identity, and not a routing key.`}</title>
    </g>
  );
}
