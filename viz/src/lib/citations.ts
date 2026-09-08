// Which declarations a verdict names.
//
// §61 of the hierarchical-semantics contract asks a proof to identify
// the declarations it consumed, "so topology-dependent proofs can be
// invalidated when the runtime architecture changes". The checker writes
// them into its prose by id — "pool.notifier_workers bounds each member
// to one simultaneously active invocation" — so the ids are already
// exact; what was missing was reading them back out. Scanning the text
// for ids the model declares recovers the citation set without the
// checker having to serialize it twice, and it is conservative in the
// right direction: an id that does not appear is never claimed, and a
// token that is not a declared id is never linked.

import type { Id } from "../types/model";
import type { Obligation } from "../types/report";

/** An id-shaped token: dotted segments, as every conseqa id is written.
 *  Trailing field paths (`input.x.request.video_id`) are trimmed back to
 *  the declared id by the prefix walk in `citedIds`. */
const TOKEN = /[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z0-9_]+)+/g;

/** The longest declared id the token starts with, or null. */
function resolve(token: string, known: ReadonlySet<Id>): Id | null {
  let candidate = token;
  for (;;) {
    if (known.has(candidate)) return candidate;
    const cut = candidate.lastIndexOf(".");
    if (cut <= 0) return null;
    candidate = candidate.slice(0, cut);
  }
}

/** Every declared id an obligation's prose names, in first-mention order. */
export function citedIds(ob: Obligation, known: ReadonlySet<Id>): Id[] {
  const seen = new Set<Id>();
  const scan = (text: string) => {
    for (const [token] of text.matchAll(TOKEN)) {
      const id = resolve(token, known);
      if (id) seen.add(id);
    }
  };
  scan(ob.summary);
  for (const a of ob.assumptions) scan(a);
  for (const e of ob.evidence) {
    if (e.subject) seen.add(e.subject);
    scan(e.message);
  }
  for (const step of ob.counterexample?.trace ?? []) {
    if (step.actor) seen.add(step.actor);
    scan(step.description);
  }
  return [...seen];
}

/** One run of a text split by the declared ids it names. */
export type TextRun = { kind: "text"; text: string } | { kind: "id"; id: Id; text: string };

/** Splits prose into plain runs and the declared ids it names, so a
 *  verdict's reasoning can be read *and* followed. */
export function splitCitations(text: string, known: ReadonlySet<Id>): TextRun[] {
  const runs: TextRun[] = [];
  let cursor = 0;
  for (const match of text.matchAll(TOKEN)) {
    const token = match[0];
    const at = match.index ?? 0;
    const id = resolve(token, known);
    if (!id) continue;
    if (at > cursor) runs.push({ kind: "text", text: text.slice(cursor, at) });
    runs.push({ kind: "id", id, text: id });
    cursor = at + id.length;
  }
  if (cursor < text.length) runs.push({ kind: "text", text: text.slice(cursor) });
  return runs;
}
