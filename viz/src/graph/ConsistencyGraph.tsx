import { useId, useMemo, useState, type KeyboardEvent } from "react";

import { CLOSURE, layoutClosure, type NodeBox, type PairGeometry } from "../lib/consistency";
import { shortId, truncate } from "../lib/ids";
import { hashes } from "../lib/route";
import { useApp } from "../state/AppState";
import type { SerializabilityView } from "../types/consistency";
import { LegendChip, LegendLine } from "./SvgCanvas";

/** Below this fraction of its natural size the drawing stops shrinking
 *  and scrolls sideways instead: the panels it lives in are narrow, and
 *  a graph whose labels cannot be read explains nothing. */
const MIN_SCALE = 0.62;
const MAX_MIN_WIDTH = 520;

interface Props {
  view: SerializabilityView;
  /** The selected pair, when the parent owns the selection. */
  selectedPair?: string | null;
  onSelectPair?: (id: string | null) => void;
}

/**
 * The conflict closure of one serializability argument, as an inline
 * drawing: the requiring transaction and every transaction it may
 * conflict with, one arrow per ordered pair standing for the
 * dependencies between them, a loop for a transaction racing a
 * concurrent execution of itself. An arrow every dependency of which a
 * declared fact commit-orders is solid; one with an unconstrained
 * dependency is dashed and amber, and when the argument fails the
 * members of the cycle it fails through wear a halo.
 *
 * Clicking a node opens the operation page with the transaction
 * selected; clicking an arrow selects the pair, so the panel around the
 * drawing can show the dependencies behind it.
 */
export function ConsistencyGraph({ view, selectedPair, onSelectPair }: Props) {
  const { navigateTo } = useApp();
  const [own, setOwn] = useState<string | null>(null);
  const controlled = selectedPair !== undefined;
  const selected = controlled ? selectedPair : own;
  const choose = (id: string) => {
    const next = id === selected ? null : id;
    if (!controlled) setOwn(next);
    onSelectPair?.(next);
  };

  const layout = useMemo(() => layoutClosure(view), [view]);
  const { viewBox } = layout;
  // Marker ids are document-wide; several closures can share a page.
  const uid = useId().replace(/[^A-Za-z0-9_-]/g, "");
  const markers = { proven: `cx-proven-${uid}`, open: `cx-open-${uid}` };
  const isolationRoute = view.route === "serializable_isolation";
  const cycle = !view.proven && view.nodes.some((n) => n.in_cycle);

  const onKey = (e: KeyboardEvent<SVGGElement>, act: () => void) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      act();
    }
  };

  return (
    <div className="space-y-1.5">
      <div className="arch-closure">
        <svg
          xmlns="http://www.w3.org/2000/svg"
          width="100%"
          viewBox={`${viewBox.x} ${viewBox.y} ${viewBox.w} ${viewBox.h}`}
          preserveAspectRatio="xMidYMid meet"
          style={{ minWidth: Math.min(viewBox.w * MIN_SCALE, MAX_MIN_WIDTH) }}
          role="img"
          aria-label={`conflict closure of ${shortId(view.transaction)}: ${view.nodes.length} transactions, ${view.pairs.length} arrows`}
        >
          <defs>
            <Marker id={markers.proven} color="var(--arch-proven)" />
            <Marker id={markers.open} color="var(--arch-unknown)" />
          </defs>

          {/* Arrows first, so the node bodies cover the arrowheads' tips. */}
          {layout.pairs.map((g) => (
            <Arrow
              key={g.pair.id}
              g={g}
              marker={g.pair.constrained ? markers.proven : markers.open}
              selected={selected === g.pair.id}
              onSelect={() => choose(g.pair.id)}
              onKey={onKey}
            />
          ))}

          {layout.nodes.map((n) => (
            <Node
              key={n.node.transaction}
              n={n}
              halo={cycle && n.node.in_cycle}
              pill={isolationRoute}
              onOpen={() => navigateTo(hashes.op(n.node.operation), `tx:${n.node.transaction}`)}
              onKey={onKey}
            />
          ))}
        </svg>
      </div>
      <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-kumo-subtle">
        <LegendLine color="var(--arch-proven)" label="commit-ordered" />
        <LegendLine color="var(--arch-unknown)" label="unconstrained" dashed />
        <LegendChip color="var(--arch-accent)" label="requiring transaction" />
        {cycle && <LegendChip color="var(--arch-unknown)" label="in an unconstrained cycle" />}
      </div>
    </div>
  );
}

function Marker({ id, color }: { id: string; color: string }) {
  return (
    <marker id={id} markerWidth={9} markerHeight={7} refX={8} refY={3.5} orient="auto" markerUnits="userSpaceOnUse">
      <path d="M0,0 L9,3.5 L0,7 Z" fill={color} />
    </marker>
  );
}

function Arrow({
  g, marker, selected, onSelect, onKey,
}: {
  g: PairGeometry;
  marker: string;
  selected: boolean;
  onSelect: () => void;
  onKey: (e: KeyboardEvent<SVGGElement>, act: () => void) => void;
}) {
  const { pair, d, labelAt, lines } = g;
  const classes = ["arch-cx-pair", pair.constrained ? "proven" : "open"];
  if (selected) classes.push("selected");
  return (
    <g
      className={classes.join(" ")}
      role="button"
      tabIndex={0}
      aria-pressed={selected}
      onClick={onSelect}
      onKeyDown={(e) => onKey(e, onSelect)}
    >
      <path className="line" d={d} markerEnd={`url(#${marker})`} />
      <path className="hit" d={d} />
      <text className="label" x={labelAt.x} y={labelAt.y - 2} textAnchor="middle">
        <tspan className="lead" x={labelAt.x} dy={0}>{lines[0]}</tspan>
        <tspan x={labelAt.x} dy={CLOSURE.LINE_H}>{lines[1]}</tspan>
      </text>
      <title>{`${shortId(pair.source)} → ${pair.self_loop ? "a concurrent execution of itself" : shortId(pair.target)}\n${pair.summary}`}</title>
    </g>
  );
}

function Node({
  n, halo, pill, onOpen, onKey,
}: {
  n: NodeBox;
  halo: boolean;
  pill: boolean;
  onOpen: () => void;
  onKey: (e: KeyboardEvent<SVGGElement>, act: () => void) => void;
}) {
  const { node } = n;
  const classes = ["arch-cx-node"];
  if (node.root) classes.push("root");
  // The subtitle names the operation and the isolation. An inline
  // transaction is conventionally named after its operation; when the
  // title already says so, the subtitle keeps its room for the isolation.
  const op = shortId(node.operation);
  const tx = shortId(node.transaction);
  const subtitle = tx === op || tx.startsWith(`${op}.`) ? node.isolation : `${op} · ${node.isolation}`;
  const pillText = "serializable";
  const pillW = pillText.length * 6 + 12;
  const title =
    `${node.transaction}\n${node.operation} · step ${node.location} · ${node.isolation}` +
    (node.root ? "\nthe requiring transaction" : "") +
    (halo ? "\nin an unconstrained cycle" : "") +
    "\n(click to open the operation page)";
  return (
    <g className={classes.join(" ")} role="link" tabIndex={0} onClick={onOpen} onKeyDown={(e) => onKey(e, onOpen)}>
      {halo && (
        <rect
          className="halo"
          x={n.x - CLOSURE.HALO}
          y={n.y - CLOSURE.HALO}
          width={n.w + 2 * CLOSURE.HALO}
          height={n.h + 2 * CLOSURE.HALO}
          rx={8 + CLOSURE.HALO}
        />
      )}
      <rect className="body" x={n.x} y={n.y} width={n.w} height={n.h} rx={8} />
      <text className="title" x={n.x + 10} y={n.y + 20}>
        {truncate(shortId(node.transaction), 24)}
      </text>
      <text className="subtitle" x={n.x + 10} y={n.y + 36}>
        {truncate(subtitle, 30)}
      </text>
      {node.root && (
        <text className="caption" x={n.x + 8} y={n.y + n.h + 12}>
          requiring
        </text>
      )}
      {pill && (
        <g>
          <rect className="pill" x={n.x + n.w - 6 - pillW} y={n.y + n.h - 9} width={pillW} height={18} rx={9} />
          <text className="pill-text" x={n.x + n.w - 6 - pillW / 2} y={n.y + n.h + 3.5} textAnchor="middle">
            {pillText}
          </text>
        </g>
      )}
      <title>{title}</title>
    </g>
  );
}
