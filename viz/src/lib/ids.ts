import type { FieldPath, Id } from "../types/model";

const KIND_PREFIXES = new Set([
  "service", "operation", "topic", "schema", "machine", "state",
  "transition", "tx", "intent", "effect", "input", "result", "output",
  "object", "data", "read", "oblig",
  // L1
  "pool", "router", "layout",
]);

/** Drops the conventional kind prefix: `operation.create_order` → `create_order`. */
export function shortId(id: string): string {
  const dot = id.indexOf(".");
  if (dot > 0 && KIND_PREFIXES.has(id.slice(0, dot))) return id.slice(dot + 1);
  return id;
}

export function truncate(text: string, max: number): string {
  return text.length > max ? text.slice(0, max - 1) + "…" : text;
}

export function pathText(path: FieldPath): string {
  return path.join(".");
}

export function wrapText(text: string, maxChars: number): string[] {
  const words = text.split(/\s+/);
  const lines: string[] = [];
  let line = "";
  for (const word of words) {
    if (line && (line + " " + word).length > maxChars) {
      lines.push(line);
      line = word;
    } else {
      line = line ? line + " " + word : word;
    }
  }
  if (line) lines.push(line);
  return lines;
}

export function encodeRoute(id: Id): string {
  return encodeURIComponent(id);
}
