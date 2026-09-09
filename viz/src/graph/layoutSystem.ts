import type { BoundaryLink, RuntimeFacts } from "../lib/runtime";
import type { Edge, Graph } from "../types/graph";
import type { Id } from "../types/model";

export interface Box {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface ServiceBox extends Box {
  id: string;
}

export interface Point {
  x: number;
  y: number;
}

export interface EdgeGeometry {
  edge: Edge;
  d: string;
  from: Point;
  to: Point;
  /** Where a label for the edge reads best. */
  labelAt: Point;
}

/** One boundary's realization, drawn as a tab on the approach into the
 *  operation it realizes — the request boundary or the subscribe edge —
 *  with a short connector to the operation card. The pool it names is a
 *  shared execution population: two boundaries naming one pool share
 *  members and nothing else, which selecting the pool makes visible. */
export interface RealizationBox extends Box {
  link: BoundaryLink;
  /** Connector from the tab to the operation's input edge. */
  connector: string;
}

/** A persistent object drawn as a downstream node, with the operations
 *  that touch it wired in. A storage layout marks it partitioned; the
 *  distinction is the point of drawing it. */
export interface DataObjectBox extends Box {
  object: Id;
  dataModel: Id;
  partitioned: boolean;
  operations: Id[];
}

/** An operation → object access, always drawn and selectable. `keyed`
 *  is what the edge is about: whether this access confines itself to a
 *  partition. */
export interface AccessEdge {
  id: string;
  operation: Id;
  object: Id;
  keyed: boolean;
  d: string;
  from: Point;
  to: Point;
}

export interface RuntimePlane {
  realizations: RealizationBox[];
  dataObjects: DataObjectBox[];
  access: AccessEdge[];
  /** Bounds of the data tier, for its heading — null when no operation
   *  touches a persistent object and there is nothing to place. */
  dataBand: Box | null;
}

export interface SystemLayout {
  pos: Map<string, Box>;
  services: ServiceBox[];
  edges: EdgeGeometry[];
  /** Null when the L1 plane is not drawn — because it is switched off, or
   *  because the model declares no runtime facts. */
  runtime: RuntimePlane | null;
  /** The L0 plane's bounds, for the layer separator. */
  l0: Box;
}

export const SYS = {
  OP_W: 210, OP_H: 64, OP_VGAP: 12,
  SVC_PAD: 14, SVC_TITLE: 30,
  /** Operations stack in one column, so every card keeps both flanks
   *  clear for its edges; only a service larger than this wraps. */
  SVC_MAX_ROWS: 8,
  COL_GAP: 130, ROW_GAP: 48,
  TOPIC_W: 220, TOPIC_H: 54,
  EXT_W: 186, EXT_H: 50,
  CLIENT_W: 150, CLIENT_H: 58,
  PORT_INSET: 14, CORNER: 16, LANE: 13, CHANNEL_GAP: 56,
  /** Vertical gap between two bands of columns, and the horizontal room
   *  outside every band that a wrapping edge travels in. */
  BAND_ROW_GAP: 110, OUTER_MARGIN: 70,
  /** The shape of the canvas the drawing is fitted into, and what an
   *  extra band has to earn back in size before it is worth taking. */
  TARGET_ASPECT: 1.8, WRAP_COST: 1.2,
  /** Below this width a drawing still fits at a readable size, so it is
   *  left on one line however wide it looks. Roughly eight columns. */
  WRAP_THRESHOLD: 2800,
  /** A realization tab: a small pill in the gutter on the approach into
   *  the operation, its right edge held this far off the card. */
  REAL_W: 126, REAL_H: 32, REAL_VGAP: 8, REAL_OFFSET: 14,
  /** The data tier below the machine: object nodes and the gap down to
   *  them from the operations that persist to them. */
  DATA_GAP: 104, DATA_TITLE: 30,
  OBJ_W: 184, OBJ_H: 58, OBJ_GAP: 30, OBJ_ROW_GAP: 40,
};

// ---------------------------------------------------------------------------
// Layering
// ---------------------------------------------------------------------------

/** A vertex of the coarse graph the columns are computed over: a whole
 *  service (its operations ride along inside it), a topic, an external
 *  system, or the clients vertex. */
interface Macro {
  id: string;
  kind: "service" | "topic" | "external" | "client";
  w: number;
  h: number;
  rank: number;
}

function serviceSize(count: number): { w: number; h: number; cols: number; rows: number } {
  const n = Math.max(1, count);
  const cols = Math.ceil(n / SYS.SVC_MAX_ROWS);
  const rows = Math.ceil(n / cols);
  return {
    cols,
    rows,
    w: SYS.SVC_PAD * 2 + cols * SYS.OP_W + (cols - 1) * SYS.SVC_PAD,
    h: SYS.SVC_TITLE + SYS.SVC_PAD + rows * SYS.OP_H + (rows - 1) * SYS.OP_VGAP,
  };
}

/** Where each column sits once the columns have been wrapped into bands. */
interface BandGeometry {
  /** Band index per column. */
  of: number[];
  /** Top of the band, where its edge channel begins. */
  top: number[];
  /** Top of the band's content, below its channel. */
  contentTop: number[];
  count: number;
}

/**
 * How many columns go in one band.
 *
 * The measure is the size everything ends up drawn at. Fitting a drawing
 * of width W and height H into a canvas of aspect A scales it by
 * `1 / max(W, H·A)`, so that quantity — smaller is bigger — is what the
 * choice minimises. It is the honest form of "use the canvas": a
 * twenty-column strip is not bad because it is wide, it is bad because
 * fitting it leaves every card too small to read.
 *
 * Wrapping costs a reader something too — an edge that leaves one band
 * and re-enters the next has to be followed — so it is not considered at
 * all until the drawing is wide enough to be unreadable when fitted, and
 * then a band must still buy at least a fifth more size to be taken.
 */
function chooseBandWidth(columnWidth: number[], columnHeight: number[], target: number): number {
  const n = columnWidth.length;
  if (n <= 1) return Math.max(1, n);

  const total = columnWidth.reduce((sum, w) => sum + w + SYS.COL_GAP, 0) - SYS.COL_GAP;
  if (total <= SYS.WRAP_THRESHOLD) return n;

  let best = n;
  let bestCost = Infinity;
  for (let perBand = n; perBand >= 1; perBand--) {
    let width = 0;
    let height = 0;
    let count = 0;
    for (let start = 0; start < n; start += perBand) {
      let bandWidth = 0;
      let bandHeight = 0;
      for (let c = start; c < Math.min(start + perBand, n); c++) {
        bandWidth += columnWidth[c] + SYS.COL_GAP;
        bandHeight = Math.max(bandHeight, columnHeight[c]);
      }
      width = Math.max(width, bandWidth - SYS.COL_GAP);
      height += bandHeight + SYS.BAND_ROW_GAP;
      count++;
    }
    height -= SYS.BAND_ROW_GAP;
    const cost = Math.max(width, height * target) * SYS.WRAP_COST ** (count - 1);
    if (cost < bestCost) {
      bestCost = cost;
      best = perBand;
    }
  }
  return best;
}

/**
 * Ranks the coarse graph left to right by longest path, ignoring the
 * edges that close a cycle.
 *
 * Cycles are ordinary here — a service publishing to the topic it also
 * subscribes to is a pipeline, not a mistake — so they are broken for
 * ranking only. The edges that were cut are still drawn; they become the
 * arcs over the plane, which is what a return path looks like.
 */
function rankMacros(nodes: Map<string, Macro>, edges: [string, string][]): void {
  const succ = new Map<string, string[]>();
  for (const id of nodes.keys()) succ.set(id, []);
  for (const [a, b] of edges) if (a !== b) succ.get(a)?.push(b);

  // Depth-first walk marking back edges: an edge into a vertex still on
  // the stack cannot be honoured by any left-to-right ordering.
  const state = new Map<string, 0 | 1 | 2>();
  const back = new Set<string>();
  const visit = (id: string) => {
    state.set(id, 1);
    for (const next of succ.get(id) ?? []) {
      const s = state.get(next) ?? 0;
      if (s === 1) back.add(`${id} ${next}`);
      else if (s === 0) visit(next);
    }
    state.set(id, 2);
  };

  const indegree = new Map<string, number>([...nodes.keys()].map((id) => [id, 0]));
  for (const [a, b] of edges) if (a !== b) indegree.set(b, (indegree.get(b) ?? 0) + 1);
  const roots = [...nodes.keys()].filter((id) => !indegree.get(id));
  for (const id of roots) if (!state.get(id)) visit(id);
  for (const id of nodes.keys()) if (!state.get(id)) visit(id);

  const forward = edges.filter(([a, b]) => a !== b && !back.has(`${a} ${b}`));

  // Longest path by relaxation. The remaining edge set is acyclic, so the
  // vertex count bounds the passes needed.
  for (let pass = 0; pass < nodes.size; pass++) {
    let changed = false;
    for (const [a, b] of forward) {
      const next = nodes.get(a)!.rank + 1;
      if (nodes.get(b)!.rank < next) {
        nodes.get(b)!.rank = next;
        changed = true;
      }
    }
    if (!changed) break;
  }
}

/** Orders each column by the mean position of its neighbours in the
 *  neighbouring columns, sweeping both ways: the standard cheap remedy
 *  for crossings, and enough for graphs of this size. */
function orderColumns(columns: Macro[][], neighbours: Map<string, string[]>): void {
  const indexOf = new Map<string, number>();
  const reindex = () => {
    for (const column of columns) column.forEach((m, i) => indexOf.set(m.id, i));
  };
  reindex();

  const sweep = (order: number[]) => {
    for (const c of order) {
      const column = columns[c];
      if (!column) continue;
      const key = new Map<string, number>();
      for (const m of column) {
        const ns = (neighbours.get(m.id) ?? [])
          .map((id) => indexOf.get(id))
          .filter((i): i is number => i !== undefined);
        key.set(m.id, ns.length ? ns.reduce((a, b) => a + b, 0) / ns.length : indexOf.get(m.id)!);
      }
      column.sort((a, b) => key.get(a.id)! - key.get(b.id)! || a.id.localeCompare(b.id));
      reindex();
    }
  };

  const forward = columns.map((_, i) => i);
  for (let pass = 0; pass < 3; pass++) {
    sweep(forward);
    sweep([...forward].reverse());
  }
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/** An axis-aligned polyline with rounded corners. */
function roundedPolyline(points: Point[], radius: number): string {
  if (points.length < 2) return "";
  let d = `M${points[0].x},${points[0].y}`;
  for (let i = 1; i < points.length - 1; i++) {
    const prev = points[i - 1];
    const corner = points[i];
    const next = points[i + 1];
    const inLen = Math.hypot(corner.x - prev.x, corner.y - prev.y);
    const outLen = Math.hypot(next.x - corner.x, next.y - corner.y);
    const r = Math.min(radius, inLen / 2, outLen / 2);
    const ux = inLen ? (corner.x - prev.x) / inLen : 0;
    const uy = inLen ? (corner.y - prev.y) / inLen : 0;
    const vx = outLen ? (next.x - corner.x) / outLen : 0;
    const vy = outLen ? (next.y - corner.y) / outLen : 0;
    d += ` L${corner.x - ux * r},${corner.y - uy * r}`;
    d += ` Q${corner.x},${corner.y} ${corner.x + vx * r},${corner.y + vy * r}`;
  }
  const last = points[points.length - 1];
  d += ` L${last.x},${last.y}`;
  return d;
}

function horizontalCubic(p1: Point, p2: Point): string {
  const dx = Math.min(160, Math.max(48, Math.abs(p2.x - p1.x) * 0.45));
  return `M${p1.x},${p1.y} C${p1.x + dx},${p1.y} ${p2.x - dx},${p2.y} ${p2.x},${p2.y}`;
}

/** Ports spread along one side of a box, ordered by where the other end
 *  of each edge sits, so incident edges arrive in the order they leave. */
function assignPorts<T>(
  items: T[],
  box: Box,
  side: "left" | "right",
  sortBy: (item: T) => number,
  keyOf: (item: T) => string,
): Map<string, Point> {
  const sorted = [...items].sort((a, b) => sortBy(a) - sortBy(b));
  const ports = new Map<string, Point>();
  const n = sorted.length;
  const inset = Math.min(SYS.PORT_INSET, box.h / (n + 1));
  const usable = box.h - inset * 2;
  const x = side === "left" ? box.x : box.x + box.w;
  sorted.forEach((item, i) => {
    const frac = n === 1 ? 0.5 : i / (n - 1);
    ports.set(keyOf(item), { x, y: box.y + inset + frac * usable });
  });
  return ports;
}

function boundsOf(boxes: Box[]): Box {
  if (!boxes.length) return { x: 0, y: 0, w: 0, h: 0 };
  const x = Math.min(...boxes.map((b) => b.x));
  const y = Math.min(...boxes.map((b) => b.y));
  const right = Math.max(...boxes.map((b) => b.x + b.w));
  const bottom = Math.max(...boxes.map((b) => b.y + b.h));
  return { x, y, w: right - x, h: bottom - y };
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

export interface LayoutOptions {
  /** The declared runtime realization, when the L1 plane is drawn. */
  runtime?: RuntimeFacts | null;
  /** The shape the drawing should aim for — the canvas's, when it is
   *  known. Columns wrap into bands to approach it. */
  aspect?: number;
}

/**
 * A layered left-to-right layout: columns follow the flow of information,
 * and everything that can happen in parallel stacks vertically inside a
 * column. A wide, one-row picture wastes the height of the canvas and
 * shrinks every card when fitted; letting the graph grow along both axes
 * is what keeps a large architecture readable.
 *
 * With the L1 plane on, the runtime realization is drawn as a band below
 * the whole L0 plane — a substrate, not another region of the same
 * drawing — because that is the relationship between the layers: L1 says
 * how the machine above it is realized.
 */
export function layoutSystem(graph: Graph, options: LayoutOptions = {}): SystemLayout {
  const runtime = options.runtime ?? null;
  const pos = new Map<string, Box>();
  const macros = new Map<string, Macro>();

  const opService = new Map(graph.operations.map((o) => [o.id, o.service]));
  const opsOf = new Map<string, string[]>(graph.services.map((s) => [s.id, []]));
  for (const op of graph.operations) {
    if (!opsOf.has(op.service)) opsOf.set(op.service, []);
    opsOf.get(op.service)!.push(op.id);
  }
  for (const [, ops] of opsOf) ops.sort();

  const macro = (vertex: string): string => opService.get(vertex) ?? vertex;

  for (const svc of graph.services) {
    const { w, h } = serviceSize(opsOf.get(svc.id)?.length ?? 0);
    macros.set(svc.id, { id: svc.id, kind: "service", w, h, rank: 0 });
  }
  for (const t of graph.topics) {
    macros.set(t.id, { id: t.id, kind: "topic", w: SYS.TOPIC_W, h: SYS.TOPIC_H, rank: 0 });
  }
  for (const e of graph.externals) {
    macros.set(e.id, { id: e.id, kind: "external", w: SYS.EXT_W, h: SYS.EXT_H, rank: 0 });
  }
  if (graph.client) {
    macros.set(graph.client.id, {
      id: graph.client.id, kind: "client", w: SYS.CLIENT_W, h: SYS.CLIENT_H, rank: 0,
    });
  }

  const macroEdges: [string, string][] = [];
  const neighbours = new Map<string, string[]>([...macros.keys()].map((id) => [id, []]));
  for (const e of graph.edges) {
    const a = macro(e.from);
    const b = macro(e.to);
    if (!macros.has(a) || !macros.has(b) || a === b) continue;
    macroEdges.push([a, b]);
    neighbours.get(a)!.push(b);
    neighbours.get(b)!.push(a);
  }

  rankMacros(macros, macroEdges);

  const maxRank = Math.max(0, ...[...macros.values()].map((m) => m.rank));
  const columns: Macro[][] = Array.from({ length: maxRank + 1 }, () => []);
  for (const m of [...macros.values()].sort((a, b) => a.id.localeCompare(b.id))) columns[m.rank].push(m);
  orderColumns(columns, neighbours);

  // Columns are as wide as their widest member. A long pipeline makes
  // many of them, and a picture twenty columns wide and three rows tall
  // fits a 16:9 canvas by shrinking every card to nothing — so the
  // columns wrap into bands, the way a paragraph wraps into lines.
  const columnWidth = columns.map((column) => Math.max(0, ...column.map((m) => m.w)));
  const columnHeight = columns.map(
    (column) => column.reduce((sum, m) => sum + m.h, 0) + Math.max(0, column.length - 1) * SYS.ROW_GAP,
  );
  const perBand = chooseBandWidth(columnWidth, columnHeight, options.aspect ?? SYS.TARGET_ASPECT);
  const bandOf = columns.map((_, c) => Math.floor(c / perBand));
  const bandCount = columns.length ? bandOf[columns.length - 1] + 1 : 1;

  // Every band reserves a channel above it for the edges that do not
  // simply cross one gutter, so those never run through a card.
  const macroColumn = new Map<string, number>();
  columns.forEach((column, c) => {
    for (const m of column) macroColumn.set(m.id, c);
  });
  const columnFor = (vertex: string) => macroColumn.get(macro(vertex)) ?? 0;
  const channelLanes = new Array<number>(bandCount).fill(0);
  for (const e of graph.edges) {
    if (!macros.has(macro(e.from)) || !macros.has(macro(e.to))) continue;
    const a = columnFor(e.from);
    const b = columnFor(e.to);
    if (bandOf[a] === bandOf[b] && Math.abs(b - a) === 1) continue;
    channelLanes[bandOf[a]]++;
    if (bandOf[a] !== bandOf[b]) channelLanes[bandOf[b]]++;
  }
  const channelSpace = channelLanes.map((n) => (n ? SYS.CHANNEL_GAP + n * SYS.LANE : SYS.ROW_GAP));

  const columnX: number[] = [];
  const bandTop: number[] = [];
  const bandContentTop: number[] = [];
  let cursorX = 0;
  let cursorY = 0;
  for (let b = 0; b < bandCount; b++) {
    const members = columns.map((_, c) => c).filter((c) => bandOf[c] === b);
    cursorX = 0;
    for (const c of members) {
      columnX[c] = cursorX;
      cursorX += columnWidth[c] + SYS.COL_GAP;
    }
    bandTop[b] = cursorY;
    bandContentTop[b] = cursorY + channelSpace[b];
    cursorY = bandContentTop[b] + Math.max(0, ...members.map((c) => columnHeight[c])) + SYS.BAND_ROW_GAP;
  }

  const columnOf = new Map<string, number>();
  columns.forEach((column, c) => {
    const band = bandOf[c];
    const tallest = Math.max(
      0,
      ...columns.map((_, other) => (bandOf[other] === band ? columnHeight[other] : 0)),
    );
    // Centred within its band, so a short column reads as part of the
    // same row as the tall one beside it.
    let y = bandContentTop[band] + (tallest - columnHeight[c]) / 2;
    for (const m of column) {
      columnOf.set(m.id, c);
      pos.set(m.id, { x: columnX[c] + (columnWidth[c] - m.w) / 2, y, w: m.w, h: m.h });
      y += m.h + SYS.ROW_GAP;
    }
  });

  const services: ServiceBox[] = [];
  for (const svc of graph.services) {
    const box = pos.get(svc.id);
    if (!box) continue;
    services.push({ id: svc.id, ...box });
    const ops = opsOf.get(svc.id) ?? [];
    const { rows } = serviceSize(ops.length);
    ops.forEach((opId, i) => {
      pos.set(opId, {
        x: box.x + SYS.SVC_PAD + Math.floor(i / rows) * (SYS.OP_W + SYS.SVC_PAD),
        y: box.y + SYS.SVC_TITLE + (i % rows) * (SYS.OP_H + SYS.OP_VGAP),
        w: SYS.OP_W,
        h: SYS.OP_H,
      });
    });
  }

  const bands: BandGeometry = { of: bandOf, top: bandTop, contentTop: bandContentTop, count: bandCount };

  // Realization vertices are placed before the edges are routed, so the
  // edges that feed a realized boundary can terminate *at* the vertex —
  // caller → [router] → operation, topic → [dispatch] → operation — with
  // the vertex a real waypoint on the path, not a tag beside its end.
  const realizations: RealizationBox[] = [];
  const retarget = new Map<string, string>();
  const vertexColumn = new Map<string, number>();
  if (runtime) {
    const byOperation = new Map<Id, BoundaryLink[]>();
    for (const link of runtime.links) {
      const list = byOperation.get(link.operation);
      if (list) list.push(link);
      else byOperation.set(link.operation, [link]);
    }
    const realized = new Set(runtime.links.map((l) => l.id));
    for (const [opId, links] of byOperation) {
      const op = pos.get(opId);
      if (!op) continue;
      links.sort((a, b) => a.input.localeCompare(b.input));
      const n = links.length;
      links.forEach((link, i) => {
        const cy = op.y + (op.h * (i + 1)) / (n + 1);
        const x = op.x - SYS.REAL_W - SYS.REAL_OFFSET;
        const y = cy - SYS.REAL_H / 2;
        realizations.push({
          link,
          x, y, w: SYS.REAL_W, h: SYS.REAL_H,
          connector: roundedPolyline(
            [{ x: x + SYS.REAL_W, y: cy }, { x: op.x - 3, y: cy }, { x: op.x, y: cy }],
            6,
          ),
        });
        pos.set(link.id, { x, y, w: SYS.REAL_W, h: SYS.REAL_H });
        vertexColumn.set(link.id, columnFor(opId));
      });
    }
    // Every edge into a realized boundary ends at its vertex.
    for (const e of graph.edges) {
      if ("input" in e) {
        const vid = `rt:${e.to}/${e.input}`;
        if (realized.has(vid)) retarget.set(e.id, vid);
      }
    }
  }

  const l0 = boundsOf([...pos.values()]);
  const edges = routeEdges(graph, pos, columnOf, macro, columnX, columnWidth, bands, retarget, vertexColumn);
  const plane = runtime
    ? layoutRuntime(runtime, pos, columnOf, macro, columnX, columnWidth, l0, realizations)
    : null;

  return { pos, services, edges, runtime: plane, l0 };
}

/**
 * Routes the information edges.
 *
 * An edge to the neighbouring column in the same band crosses the gutter
 * directly, in either direction. A longer one — a leap over columns, a
 * return to an earlier one, a hop within one — leaves through the side
 * it is headed for, climbs a column gutter into the channel reserved
 * above its band, and comes back down another gutter. An edge that
 * crosses bands does the same and travels between them outside every
 * band, which is the only column of space guaranteed to be empty.
 *
 * Risers stay in gutters and channels for the same reason: a line drawn
 * straight from card to card would cross whatever lies between them, and
 * a diagram whose edges pass through its nodes cannot be read.
 */
function routeEdges(
  graph: Graph,
  pos: Map<string, Box>,
  columnOf: Map<string, number>,
  macro: (vertex: string) => string,
  columnX: number[],
  columnWidth: number[],
  bands: BandGeometry,
  /** Edges whose destination is a realization vertex rather than the
   *  operation itself — the vertex sits on the path into the boundary. */
  retarget: Map<string, string>,
  vertexColumn: Map<string, number>,
): EdgeGeometry[] {
  const columnFor = (vertex: string) => columnOf.get(macro(vertex)) ?? 0;
  // Where an edge actually ends: its realization vertex, or its own `to`.
  const dst = (e: Edge) => retarget.get(e.id) ?? e.to;
  const dstColumn = (e: Edge) => {
    const vid = retarget.get(e.id);
    return vid !== undefined ? vertexColumn.get(vid)! : columnFor(e.to);
  };
  type Mode = "direct" | "channel" | "cross";
  type Side = "left" | "right";

  const plan = new Map<string, { mode: Mode; from: Side; to: Side; span: number }>();
  for (const e of graph.edges) {
    if (!pos.has(e.from) || !pos.has(dst(e))) continue;
    const a = columnFor(e.from);
    const b = dstColumn(e);
    const forward = b > a;
    if (bands.of[a] !== bands.of[b]) {
      plan.set(e.id, {
        mode: "cross",
        from: forward ? "right" : "left",
        to: forward ? "left" : "right",
        span: Math.abs(b - a),
      });
    } else if (Math.abs(b - a) === 1) {
      plan.set(e.id, { mode: "direct", from: forward ? "right" : "left", to: forward ? "left" : "right", span: 1 });
    } else {
      plan.set(e.id, {
        mode: "channel",
        from: forward ? "right" : "left",
        to: forward ? "left" : "right",
        span: Math.max(1, Math.abs(b - a)),
      });
    }
  }

  const centre = (vertex: string) => {
    const box = pos.get(vertex);
    return box ? box.y + box.h / 2 : 0;
  };

  const ports = new Map<string, Point>();
  const bySide = new Map<string, { left: Edge[]; right: Edge[] }>();
  const bucket = (id: string) => {
    let b = bySide.get(id);
    if (!b) {
      b = { left: [], right: [] };
      bySide.set(id, b);
    }
    return b;
  };
  for (const e of graph.edges) {
    const p = plan.get(e.id);
    if (!p) continue;
    bucket(e.from)[p.from].push(e);
    bucket(dst(e))[p.to].push(e);
  }
  for (const [id, sides] of bySide) {
    const box = pos.get(id)!;
    for (const side of ["left", "right"] as const) {
      const assigned = assignPorts(
        sides[side],
        box,
        side,
        (e) => centre(e.from === id ? e.to : e.from),
        (e) => `${e.id} ${id}`,
      );
      for (const [key, point] of assigned) ports.set(key, point);
    }
  }

  // Risers share a gutter but never an x: each takes its own slot, spread
  // outwards from the middle of the gap between two columns.
  const used = new Map<string, number>();
  const riserX = (column: number, side: Side) => {
    const gutter =
      side === "left"
        ? columnX[column] - SYS.COL_GAP / 2
        : columnX[column] + columnWidth[column] + SYS.COL_GAP / 2;
    const key = `${bands.of[column]}:${gutter}`;
    const n = used.get(key) ?? 0;
    used.set(key, n + 1);
    const step = Math.min(Math.ceil((n + 1) / 2) * 14, SYS.COL_GAP / 2 - 12);
    return gutter + step * (n % 2 === 0 ? -1 : 1);
  };

  // Lanes within each band's channel, longest edges highest, so a leap
  // across the drawing rides above the short returns rather than through
  // them.
  const laneUsed = new Array<number>(bands.count).fill(0);
  const lane = new Map<string, number>();
  const channelY = (band: number, key: string) => {
    let n = lane.get(key);
    if (n === undefined) {
      n = laneUsed[band]++;
      lane.set(key, n);
    }
    return bands.contentTop[band] - 24 - n * SYS.LANE;
  };

  const right = Math.max(...columnX.map((x, c) => x + columnWidth[c])) + SYS.OUTER_MARGIN;
  const left = Math.min(...columnX) - SYS.OUTER_MARGIN;

  const ordered = [...graph.edges].sort(
    (a, b) => (plan.get(b.id)?.span ?? 0) - (plan.get(a.id)?.span ?? 0) || a.id.localeCompare(b.id),
  );
  const geometry = new Map<string, EdgeGeometry>();
  for (const e of ordered) {
    const p = plan.get(e.id);
    const p1 = ports.get(`${e.id} ${e.from}`);
    const p2 = ports.get(`${e.id} ${dst(e)}`);
    if (!p || !p1 || !p2) continue;

    if (p.mode === "direct") {
      geometry.set(e.id, {
        edge: e,
        d: horizontalCubic(p1, p2),
        from: p1,
        to: p2,
        labelAt: { x: (p1.x + p2.x) / 2, y: (p1.y + p2.y) / 2 - 8 },
      });
      continue;
    }

    const columnA = columnFor(e.from);
    const columnB = dstColumn(e);
    const rise = riserX(columnA, p.from);
    const fall = riserX(columnB, p.to);

    if (p.mode === "channel") {
      const yc = channelY(bands.of[columnA], `${e.id} c`);
      geometry.set(e.id, {
        edge: e,
        d: roundedPolyline(
          [p1, { x: rise, y: p1.y }, { x: rise, y: yc }, { x: fall, y: yc }, { x: fall, y: p2.y }, p2],
          SYS.CORNER,
        ),
        from: p1,
        to: p2,
        labelAt: { x: (rise + fall) / 2, y: yc - 8 },
      });
      continue;
    }

    // Across bands: out through the band's channel, down the outside,
    // back in through the target band's channel — the way a line of text
    // wraps, and for the same reason.
    const yA = channelY(bands.of[columnA], `${e.id} a`);
    const yB = channelY(bands.of[columnB], `${e.id} b`);
    const margin = p.from === "right" ? right : left;
    geometry.set(e.id, {
      edge: e,
      d: roundedPolyline(
        [
          p1,
          { x: rise, y: p1.y },
          { x: rise, y: yA },
          { x: margin, y: yA },
          { x: margin, y: yB },
          { x: fall, y: yB },
          { x: fall, y: p2.y },
          p2,
        ],
        SYS.CORNER,
      ),
      from: p1,
      to: p2,
      labelAt: { x: margin, y: (yA + yB) / 2 },
    });
  }

  return graph.edges.map((e) => geometry.get(e.id)).filter((g): g is EdgeGeometry => !!g);
}

/**
 * The data tier, wired to the machine above it.
 *
 * The realization vertices are already placed on the paths they belong
 * to (a router or a dispatch is a fact about a boundary, so it lives on
 * that boundary's edge). What is left is storage: a layout is a fact
 * about an object, so the objects operations persist to are drawn as a
 * downstream tier and wired to the operations that touch them. Each
 * access edge carries the one fact that matters of it — whether the
 * access keys to the partition — and every edge can be selected.
 */
function layoutRuntime(
  runtime: RuntimeFacts,
  pos: Map<string, Box>,
  columnOf: Map<string, number>,
  macro: (vertex: string) => string,
  columnX: number[],
  columnWidth: number[],
  l0: Box,
  realizations: RealizationBox[],
): RuntimePlane {
  const centre = l0.x + l0.w / 2;
  const centreX = (id: string) => {
    const b = pos.get(id);
    return b ? b.x + b.w / 2 : centre;
  };

  // Object nodes below the machine, placed under the mean of the
  // operations that touch them and pushed apart in that order, wrapped
  // once a row reaches the width of the plane above.
  const anchorOf = (ops: Id[]) =>
    ops.length ? ops.reduce((sum, o) => sum + centreX(o), 0) / ops.length : centre;
  const objects = [...runtime.dataObjects].sort(
    (a, b) => anchorOf(a.operations) - anchorOf(b.operations) || a.object.localeCompare(b.object),
  );
  const limit = Math.max(l0.x + l0.w, l0.x + SYS.WRAP_THRESHOLD);
  const dataTop = l0.y + l0.h + SYS.DATA_GAP;
  const dataObjects: DataObjectBox[] = [];
  let cursor = l0.x;
  let rowTop = dataTop;
  for (const obj of objects) {
    let x = Math.max(anchorOf(obj.operations) - SYS.OBJ_W / 2, cursor);
    if (x + SYS.OBJ_W > limit && cursor > l0.x) {
      rowTop += SYS.OBJ_H + SYS.OBJ_ROW_GAP;
      cursor = l0.x;
      x = l0.x;
    }
    cursor = x + SYS.OBJ_W + SYS.OBJ_GAP;
    const box: DataObjectBox = {
      object: obj.object,
      dataModel: obj.dataModel,
      partitioned: obj.partitioned,
      operations: obj.operations,
      x, y: rowTop, w: SYS.OBJ_W, h: SYS.OBJ_H,
    };
    dataObjects.push(box);
    pos.set(box.object, { x: box.x, y: box.y, w: box.w, h: box.h });
  }

  // Access edges leave the operation's foot, drop through the gutter
  // beside its column to a trunk below the machine, and rise into the
  // object — kept out of the cards between, the way every other long
  // edge here is.
  const objectBox = new Map(dataObjects.map((b) => [b.object, b]));
  const trunkY = l0.y + l0.h + SYS.DATA_GAP - SYS.CORNER;
  const access: AccessEdge[] = [];
  for (const fact of runtime.access) {
    const op = pos.get(fact.operation);
    const obj = objectBox.get(fact.object);
    if (!op || !obj) continue;
    const column = columnOf.get(macro(fact.operation)) ?? 0;
    const gutter = columnX[column] + columnWidth[column] + SYS.COL_GAP / 2;
    const from = { x: op.x + op.w * 0.5, y: op.y + op.h };
    const to = { x: obj.x + obj.w / 2, y: obj.y };
    access.push({
      id: fact.id,
      operation: fact.operation,
      object: fact.object,
      keyed: fact.keyed,
      from,
      to,
      d: roundedPolyline(
        [from, { x: gutter, y: from.y }, { x: gutter, y: trunkY }, { x: to.x, y: trunkY }, to],
        SYS.CORNER,
      ),
    });
  }

  const dataBand = dataObjects.length ? boundsOf(dataObjects) : null;
  return { realizations, dataObjects, access, dataBand };
}
