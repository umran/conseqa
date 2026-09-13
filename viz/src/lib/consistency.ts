// The transaction consistency arguments, read the way the panels and
// the drawings need them.
//
// The page data carries one structured argument per declared
// serializability or ordering requirement (`types/consistency.ts`). This
// module indexes them by the obligation they belong to and lays the
// conflict closure out as a small graph: the requiring transaction and
// every transaction that may conflict with it, one arrow per ordered
// pair standing for the dependencies between them, and a loop for a
// transaction racing a concurrent execution of itself. Pure functions
// throughout, so the geometry can be checked without a browser.

import type {
  ClosureNode, OrderingView, PairView, SerializabilityView,
} from "../types/consistency";
import type { Id, RequirementKind } from "../types/model";
import type { PageData } from "../types/page";
import type { Obligation } from "../types/report";
import { shortId, truncate } from "./ids";

// ---------------------------------------------------------------------------
// Lookups
// ---------------------------------------------------------------------------

export type ObligationProof =
  | { kind: "serializability"; view: SerializabilityView }
  | { kind: "ordering"; view: OrderingView };

export interface TransactionViews {
  serializability: SerializabilityView[];
  ordering: OrderingView[];
}

export interface ConsistencyViews {
  serializability: SerializabilityView[];
  ordering: OrderingView[];
  serializabilityFor(obligationId: string): SerializabilityView | null;
  orderingFor(obligationId: string): OrderingView | null;
  /** Both families declared on one transaction, in requirement order. */
  viewsForTransaction(operation: Id, transaction: Id): TransactionViews;
  /** The argument behind an obligation of a transaction family, by id. */
  proofForObligation(ob: Obligation): ObligationProof | null;
  /** The argument behind a declared requirement, addressed the way the
   *  requirements table addresses it. */
  proofForRequirement(operation: Id, transaction: Id, prop: RequirementKind, index: number): ObligationProof | null;
}

/** The checker's obligation id for a transaction-family requirement:
 *  `oblig.<operation>.<transaction>.<family>.<index>`. */
export function obligationId(operation: Id, transaction: Id, prop: RequirementKind, index: number): string {
  return `oblig.${operation}.${transaction}.${prop}.${index}`;
}

export function consistencyViews(data: PageData): ConsistencyViews {
  // Tolerate a page written before the arguments were emitted: no
  // argument is then the absence of a drawing, never a broken page.
  const serializability = data.consistency?.serializability ?? [];
  const ordering = data.consistency?.ordering ?? [];
  const byS = new Map(serializability.map((v) => [v.obligation, v]));
  const byO = new Map(ordering.map((v) => [v.obligation, v]));

  const serializabilityFor = (id: string) => byS.get(id) ?? null;
  const orderingFor = (id: string) => byO.get(id) ?? null;

  const proofForRequirement = (operation: Id, transaction: Id, prop: RequirementKind, index: number): ObligationProof | null => {
    const id = obligationId(operation, transaction, prop, index);
    if (prop === "transaction_serializability") {
      const view = serializabilityFor(id);
      return view ? { kind: "serializability", view } : null;
    }
    if (prop === "transaction_ordering") {
      const view = orderingFor(id);
      return view ? { kind: "ordering", view } : null;
    }
    return null;
  };

  return {
    serializability,
    ordering,
    serializabilityFor,
    orderingFor,
    viewsForTransaction: (operation, transaction) => ({
      serializability: serializability
        .filter((v) => v.operation === operation && v.transaction === transaction)
        .sort((a, b) => a.requirement - b.requirement),
      ordering: ordering
        .filter((v) => v.operation === operation && v.transaction === transaction)
        .sort((a, b) => a.requirement - b.requirement),
    }),
    proofForObligation: (ob) => {
      if (ob.property.kind === "transaction_serializability") {
        const view = serializabilityFor(ob.id);
        return view ? { kind: "serializability", view } : null;
      }
      if (ob.property.kind === "transaction_ordering") {
        const view = orderingFor(ob.id);
        return view ? { kind: "ordering", view } : null;
      }
      return null;
    },
    proofForRequirement,
  };
}

/** The route a serializability argument took, in a few words. */
export function routeText(view: SerializabilityView): string {
  switch (view.route) {
    case "serializable_isolation":
      return "serializable isolation";
    case "conflict_graph":
      return "serialization graph";
    case null:
      return view.cycles.length ? "cycle unconstrained" : "no route";
  }
}

/** One phrase for a requirement row: what the argument rests on, or
 *  where it stops. */
export function proofSummary(proof: ObligationProof): string {
  if (proof.kind === "serializability") {
    return `closure of ${proof.view.nodes.length} · ${routeText(proof.view)}`;
  }
  const { view } = proof;
  const m = view.mechanism;
  if (!m) return "no cursor or fence";
  const guard = m.kind === "cursor" ? `cursor · ${m.rule ?? "cursor"}` : "fence";
  if (!m.carries_position) return `${guard} · not on the position`;
  if (!view.serializability.proven) return `${guard} · closure unproven`;
  return guard;
}

/** The two lines an arrow of the closure graph carries: what it stands
 *  for, and what orders it — or what is missing. A constrained arrow
 *  names its first kind of evidence and counts the rest; the full list
 *  is the arrow's hover summary and the dependency rows behind it. */
export function pairLabel(pair: PairView): [string, string] {
  const objects = pair.objects.map(shortId).join(", ");
  const lead = `${pair.dependencies} dep · ${truncate(objects, 24)}`;
  let detail: string;
  if (pair.constrained) {
    const [first, ...rest] = pair.evidence;
    detail = first === undefined ? "commit-ordered" : rest.length ? `${first} +${rest.length}` : first;
  } else {
    detail = `${pair.open} open · ${pair.gaps[0] ?? "unconstrained"}`;
  }
  return [lead, detail];
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

export interface Point {
  x: number;
  y: number;
}

export interface Box {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface NodeBox extends Box {
  node: ClosureNode;
  /** The top corner a self-loop sits on: the one facing away from the
   *  rest of the closure, so the loop never crosses the arrows that
   *  leave towards it. */
  loopCorner: "left" | "right";
}

export interface PairGeometry {
  pair: PairView;
  d: string;
  /** Where the label's two lines are centred. */
  labelAt: Point;
  lines: [string, string];
}

export interface ClosureLayout {
  nodes: NodeBox[];
  pairs: PairGeometry[];
  viewBox: Box;
}

export const CLOSURE = {
  NODE_W: 176, NODE_H: 48,
  /** Two nodes sit side by side this far apart; three or more sit on a
   *  ring of at least this radius, which grows so neighbours never touch. */
  GAP: 150, MIN_RADIUS: 150, RING_CLEAR: 60,
  PAD: 24,
  /** A two-way pair is two curves bowed to opposite sides; each leaves
   *  its node one lane off the centre line so the two do not share an
   *  endpoint. */
  BOW: 30, LANE: 9,
  /** A self-loop is a three-quarter circle over a top corner. */
  LOOP_R: 28,
  /** The arrowhead's tip sits this far short of the node border. */
  TIP: 2,
  /** Label metrics, for placing labels clear of their curve and for the
   *  viewBox: an average character of the 11px label face, and one
   *  line's height. */
  CHAR_W: 5.7, LINE_H: 12.5, LABEL_CLEAR: 8,
  /** Room below a node for the requiring caption and the isolation pill. */
  FOOT: 18,
  /** Halo drawn around a node of an unconstrained cycle. */
  HALO: 5,
};

function centre(b: Box): Point {
  return { x: b.x + b.w / 2, y: b.y + b.h / 2 };
}

/** Where a ray from a point inside a box leaves it. */
function rayExit(origin: Point, dir: Point, box: Box): Point {
  const tx = dir.x > 0 ? (box.x + box.w - origin.x) / dir.x : dir.x < 0 ? (box.x - origin.x) / dir.x : Infinity;
  const ty = dir.y > 0 ? (box.y + box.h - origin.y) / dir.y : dir.y < 0 ? (box.y - origin.y) / dir.y : Infinity;
  const t = Math.max(0, Math.min(tx, ty));
  return { x: origin.x + dir.x * t, y: origin.y + dir.y * t };
}

function fmt(p: Point): string {
  return `${Math.round(p.x * 10) / 10},${Math.round(p.y * 10) / 10}`;
}

function labelWidth(lines: string[]): number {
  return Math.max(...lines.map((l) => l.length)) * CLOSURE.CHAR_W;
}

/** The label's bounding box: two lines centred on the anchor. */
function labelBox(at: Point, lines: string[]): Box {
  const w = labelWidth(lines);
  return { x: at.x - w / 2, y: at.y - CLOSURE.LINE_H + 1, w, h: CLOSURE.LINE_H * 2 };
}

function union(boxes: Box[]): Box {
  if (!boxes.length) return { x: 0, y: 0, w: 0, h: 0 };
  const x = Math.min(...boxes.map((b) => b.x));
  const y = Math.min(...boxes.map((b) => b.y));
  const right = Math.max(...boxes.map((b) => b.x + b.w));
  const bottom = Math.max(...boxes.map((b) => b.y + b.h));
  return { x, y, w: right - x, h: bottom - y };
}

function pointsBox(points: Point[]): Box {
  return union(points.map((p) => ({ x: p.x, y: p.y, w: 0, h: 0 })));
}

/** Places the closure: one node centred; two side by side; three or
 *  more on a ring with the requiring transaction at the left. */
export function placeClosure(nodes: ClosureNode[]): NodeBox[] {
  const { NODE_W: W, NODE_H: H } = CLOSURE;
  // The requiring transaction leads, whatever order the nodes arrive in.
  const ordered = [...nodes].sort((a, b) => Number(b.root) - Number(a.root));
  const n = ordered.length;
  const centres: Point[] = [];
  if (n === 1) {
    centres.push({ x: 0, y: 0 });
  } else if (n === 2) {
    const half = (W + CLOSURE.GAP) / 2;
    centres.push({ x: -half, y: 0 }, { x: half, y: 0 });
  } else {
    // Neighbours on the ring are a chord apart; the chord must clear a
    // node's width, so the radius grows with the count.
    const chord = W + CLOSURE.RING_CLEAR;
    const r = Math.max(CLOSURE.MIN_RADIUS, chord / (2 * Math.sin(Math.PI / n)));
    for (let i = 0; i < n; i++) {
      const a = Math.PI + (2 * Math.PI * i) / n;
      centres.push({ x: r * Math.cos(a), y: r * Math.sin(a) });
    }
  }
  return ordered.map((node, i) => {
    const c = centres[i];
    // Every placement is centred on the origin, so a node left of it
    // faces the rest of the closure with its right side.
    const loopCorner: "left" | "right" = n >= 2 && c.x < -1 ? "left" : "right";
    return { node, x: c.x - W / 2, y: c.y - H / 2, w: W, h: H, loopCorner };
  });
}

/** An arrow between two distinct nodes: a straight line when it is the
 *  only direction, else a quadratic bowed to the right of travel so the
 *  partner arrow bows to the other side. */
function chord(a: Box, b: Box, bowed: boolean, lines: [string, string]): { d: string; labelAt: Point; extent: Box } {
  const ca = centre(a);
  const cb = centre(b);
  const len = Math.hypot(cb.x - ca.x, cb.y - ca.y) || 1;
  const dir = { x: (cb.x - ca.x) / len, y: (cb.y - ca.y) / len };
  // Right of travel, on a screen whose y axis points down.
  const n = { x: -dir.y, y: dir.x };
  const lane = bowed ? CLOSURE.LANE : 0;
  const p0 = rayExit({ x: ca.x + n.x * lane, y: ca.y + n.y * lane }, dir, a);
  const exit = rayExit({ x: cb.x + n.x * lane, y: cb.y + n.y * lane }, { x: -dir.x, y: -dir.y }, b);
  const p2 = { x: exit.x - dir.x * CLOSURE.TIP, y: exit.y - dir.y * CLOSURE.TIP };
  const mid = { x: (p0.x + p2.x) / 2, y: (p0.y + p2.y) / 2 };

  let d: string;
  let on: Point;
  let extent: Box;
  if (bowed) {
    const c = { x: mid.x + n.x * CLOSURE.BOW, y: mid.y + n.y * CLOSURE.BOW };
    d = `M${fmt(p0)} Q${fmt(c)} ${fmt(p2)}`;
    // The curve's midpoint is halfway to the control point.
    on = { x: mid.x + (n.x * CLOSURE.BOW) / 2, y: mid.y + (n.y * CLOSURE.BOW) / 2 };
    extent = pointsBox([p0, c, p2]);
  } else {
    d = `M${fmt(p0)} L${fmt(p2)}`;
    on = mid;
    extent = pointsBox([p0, p2]);
  }
  // The label sits beside the curve's midpoint, far enough along the
  // normal that its own extent — wide when the normal is horizontal,
  // tall when it is vertical — stays clear of the line.
  const w = labelWidth(lines);
  const clear =
    Math.abs(n.x) * (w / 2 + CLOSURE.LABEL_CLEAR) + Math.abs(n.y) * (CLOSURE.LINE_H + CLOSURE.LABEL_CLEAR);
  const labelAt = { x: on.x + n.x * clear, y: on.y + n.y * clear };
  return { d, labelAt, extent };
}

/** A transaction racing a concurrent execution of itself: a
 *  three-quarter circle over the node's outward top corner, entering
 *  the node again on its side. The label sits above the loop; on a
 *  ring it is centred on the loop, clear of the chords that pass the
 *  node, while a lone node or a pair has nothing above it and the label
 *  centres on the node instead, which keeps the drawing narrow. */
function loop(box: NodeBox, overNode: boolean): { d: string; labelAt: Point; extent: Box } {
  const r = CLOSURE.LOOP_R;
  const cy = box.y;
  const cx = box.loopCorner === "right" ? box.x + box.w : box.x;
  const d =
    box.loopCorner === "right"
      ? `M${fmt({ x: cx - r, y: cy })} A${r},${r} 0 1 1 ${fmt({ x: cx, y: cy + r })}`
      : `M${fmt({ x: cx + r, y: cy })} A${r},${r} 0 1 0 ${fmt({ x: cx, y: cy + r })}`;
  return {
    d,
    labelAt: { x: overNode ? box.x + box.w / 2 : cx, y: cy - r - CLOSURE.LINE_H - 2 },
    extent: { x: cx - r, y: cy - r, w: 2 * r, h: 2 * r },
  };
}

/** The closure as a drawing: node boxes, one arrow per pair, and the
 *  viewBox that holds them with their labels. */
export function layoutClosure(view: SerializabilityView): ClosureLayout {
  const nodes = placeClosure(view.nodes);
  const byTx = new Map(nodes.map((n) => [n.node.transaction, n]));
  const twoWay = new Set(
    view.pairs
      .filter((p) => !p.self_loop && view.pairs.some((q) => !q.self_loop && q.source === p.target && q.target === p.source))
      .map((p) => p.id),
  );

  const extents: Box[] = nodes.map((n) => ({
    x: n.x - CLOSURE.HALO,
    y: n.y - CLOSURE.HALO,
    w: n.w + 2 * CLOSURE.HALO,
    h: n.h + 2 * CLOSURE.HALO + CLOSURE.FOOT,
  }));

  const pairs: PairGeometry[] = [];
  for (const pair of view.pairs) {
    const a = byTx.get(pair.source);
    const b = byTx.get(pair.target);
    if (!a || !b) continue;
    const lines = pairLabel(pair);
    const g = pair.self_loop || a === b ? loop(a, nodes.length <= 2) : chord(a, b, twoWay.has(pair.id), lines);
    pairs.push({ pair, d: g.d, labelAt: g.labelAt, lines });
    extents.push(g.extent, labelBox(g.labelAt, lines));
  }

  const bounds = union(extents);
  const viewBox = {
    x: bounds.x - CLOSURE.PAD,
    y: bounds.y - CLOSURE.PAD,
    w: bounds.w + 2 * CLOSURE.PAD,
    h: bounds.h + 2 * CLOSURE.PAD,
  };
  return { nodes, pairs, viewBox };
}
