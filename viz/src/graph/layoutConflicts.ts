// The transaction conflict overlay of the system view.
//
// A serializability argument is about transactions; the system view is
// about operations. Between the two operations that own two transactions
// of one closure there is contention: their committed executions may
// have to be ordered against each other. The overlay draws that as one
// arc per unordered pair of operations, merged over every argument and
// both directions, green when every dependency it stands for is
// commit-ordered by a declared fact and amber when one is not — and as
// a loop on an operation whose transaction may conflict with a
// concurrent execution of itself. Contention is mutual, so no arc has
// an arrowhead. An L0 reading of the model: no runtime fact is drawn or
// consulted here.

import { shortId } from "../lib/ids";
import type { SerializabilityView } from "../types/transactionProofs";
import type { Id } from "../types/model";
import type { Box, Point } from "./layoutSystem";

export interface ConflictArc {
  /** Selection key: `cx:<opA>:<opB>` for an arc, `cx:<op>` for a loop. */
  key: string;
  a: Id;
  b: Id;
  loop: boolean;
  /** Every merged dependency is commit-ordered by a declared fact. */
  constrained: boolean;
  objects: Id[];
  /** One line per merged pair, for the tooltip. */
  summaries: string[];
  /** The first argument that produced the arc — what selecting it opens. */
  view: SerializabilityView;
  d: string;
}

export const CONFLICT = {
  /** A bracket beside two cards of one column reaches this far out,
   *  plus a share of the vertical distance so brackets nest. */
  SIDE_REACH: 46, SIDE_SLOPE: 0.22,
  /** An arc over the plane rises this far above the higher card, plus a
   *  share of the horizontal distance, capped. */
  RISE: 60, RISE_SLOPE: 0.2, MAX_RISE: 260,
  /** Attachment points spread along an edge stay this far from its ends. */
  PORT_INSET: 12,
  /** The loop glyph at a card's top-right corner, beside the status chip. */
  LOOP_R: 9, LOOP_DX: 12, LOOP_DY: -2,
};

interface Merged {
  key: string;
  a: Id;
  b: Id;
  loop: boolean;
  constrained: boolean;
  objects: Set<Id>;
  summaries: Set<string>;
  view: SerializabilityView;
}

/** Every pair of every argument, folded onto the operations that own
 *  the transactions: one entry per unordered operation pair, one per
 *  operation for the pairs within it. */
export function mergeConflicts(views: SerializabilityView[]): Merged[] {
  const merged = new Map<string, Merged>();
  for (const view of views) {
    const opOf = new Map(view.nodes.map((n) => [n.transaction, n.operation]));
    for (const pair of view.pairs) {
      const a = opOf.get(pair.source);
      const b = opOf.get(pair.target);
      if (!a || !b) continue;
      // Two transactions of one operation contend the way one
      // transaction contends with itself: within the operation.
      const loop = a === b;
      const [lo, hi] = a < b ? [a, b] : [b, a];
      const key = loop ? `cx:${a}` : `cx:${lo}:${hi}`;
      let entry = merged.get(key);
      if (!entry) {
        entry = { key, a: lo, b: hi, loop, constrained: true, objects: new Set(), summaries: new Set(), view };
        merged.set(key, entry);
      }
      entry.constrained = entry.constrained && pair.constrained;
      for (const o of pair.objects) entry.objects.add(o);
      entry.summaries.add(`${shortId(pair.source)} → ${shortId(pair.target)}: ${pair.summary}`);
    }
  }
  return [...merged.values()];
}

function fmt(p: Point): string {
  return `${Math.round(p.x * 10) / 10},${Math.round(p.y * 10) / 10}`;
}

/** Spreads the arcs touching one card along one of its edges, ordered
 *  so that the arc to the farther partner takes the outer position and
 *  brackets nest instead of crossing. */
function spread(
  entries: { key: string; offset: number }[],
  from: number,
  to: number,
): Map<string, number> {
  const sorted = [...entries].sort((p, q) => p.offset - q.offset || p.key.localeCompare(q.key));
  const out = new Map<string, number>();
  const n = sorted.length;
  sorted.forEach((e, i) => {
    const frac = n === 1 ? 0.5 : i / (n - 1);
    out.set(e.key, from + frac * (to - from));
  });
  return out;
}

/** The overlay's geometry over the system layout's card positions. */
export function layoutConflicts(views: SerializabilityView[], pos: Map<string, Box>): ConflictArc[] {
  const merged = mergeConflicts(views).filter((m) => pos.has(m.a) && pos.has(m.b));

  // Two cards that overlap horizontally sit in one column, one above
  // the other: the arc between them is a bracket on their right side.
  // Otherwise it rises from the top of one card over the plane to the
  // top of the other.
  const mode = (m: Merged): "loop" | "side" | "top" => {
    if (m.loop) return "loop";
    const a = pos.get(m.a)!;
    const b = pos.get(m.b)!;
    return a.x < b.x + b.w && b.x < a.x + a.w ? "side" : "top";
  };

  // Attachment points per card, spread along the edge the arc uses.
  const sidePorts = new Map<Id, { key: string; offset: number }[]>();
  const topPorts = new Map<Id, { key: string; offset: number }[]>();
  const note = (map: Map<Id, { key: string; offset: number }[]>, op: Id, entry: { key: string; offset: number }) => {
    const list = map.get(op);
    if (list) list.push(entry);
    else map.set(op, [entry]);
  };
  for (const m of merged) {
    const kind = mode(m);
    if (kind === "loop") continue;
    const a = pos.get(m.a)!;
    const b = pos.get(m.b)!;
    if (kind === "side") {
      note(sidePorts, m.a, { key: m.key, offset: b.y - a.y });
      note(sidePorts, m.b, { key: m.key, offset: a.y - b.y });
    } else {
      note(topPorts, m.a, { key: m.key, offset: b.x - a.x });
      note(topPorts, m.b, { key: m.key, offset: a.x - b.x });
    }
  }
  const sideY = new Map<Id, Map<string, number>>();
  for (const [op, entries] of sidePorts) {
    const box = pos.get(op)!;
    sideY.set(op, spread(entries, box.y + CONFLICT.PORT_INSET, box.y + box.h - CONFLICT.PORT_INSET));
  }
  const topX = new Map<Id, Map<string, number>>();
  for (const [op, entries] of topPorts) {
    const box = pos.get(op)!;
    topX.set(op, spread(entries, box.x + 20, box.x + box.w - 20));
  }

  return merged.map((m) => {
    const a = pos.get(m.a)!;
    const b = pos.get(m.b)!;
    let d: string;
    switch (mode(m)) {
      case "loop": {
        const r = CONFLICT.LOOP_R;
        const cx = a.x + a.w + CONFLICT.LOOP_DX;
        const cy = a.y + CONFLICT.LOOP_DY;
        d = `M${fmt({ x: cx - r, y: cy })} A${r},${r} 0 1 1 ${fmt({ x: cx, y: cy + r })}`;
        break;
      }
      case "side": {
        const y1 = sideY.get(m.a)!.get(m.key)!;
        const y2 = sideY.get(m.b)!.get(m.key)!;
        const x1 = a.x + a.w;
        const x2 = b.x + b.w;
        const reach = Math.max(x1, x2) + CONFLICT.SIDE_REACH + CONFLICT.SIDE_SLOPE * Math.abs(y2 - y1);
        d = `M${fmt({ x: x1, y: y1 })} C${fmt({ x: reach, y: y1 })} ${fmt({ x: reach, y: y2 })} ${fmt({ x: x2, y: y2 })}`;
        break;
      }
      case "top": {
        const x1 = topX.get(m.a)!.get(m.key)!;
        const x2 = topX.get(m.b)!.get(m.key)!;
        const rise = Math.min(CONFLICT.MAX_RISE, CONFLICT.RISE + CONFLICT.RISE_SLOPE * Math.abs(x2 - x1));
        const cy = Math.min(a.y, b.y) - rise;
        d = `M${fmt({ x: x1, y: a.y })} Q${fmt({ x: (x1 + x2) / 2, y: cy })} ${fmt({ x: x2, y: b.y })}`;
        break;
      }
    }
    return {
      key: m.key,
      a: m.a,
      b: m.b,
      loop: m.loop,
      constrained: m.constrained,
      objects: [...m.objects].sort(),
      summaries: [...m.summaries],
      view: m.view,
      d,
    };
  });
}
