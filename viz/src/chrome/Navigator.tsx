import { Input } from "@cloudflare/kumo/components/input";
import { CaretRightIcon, GraphIcon } from "@phosphor-icons/react";
import { useEffect, useMemo, useState } from "react";

import {
  PAGE_KIND_LABEL, navigationTree, pathId, routePath, type NavGroup, type NavNode, type PageKind,
} from "../lib/navigation";
import { STATUS_ORDER, worstStatus } from "../lib/obligations";
import { hashes } from "../lib/route";
import { useApp } from "../state/AppState";
import type { Status } from "../types/report";

/** The kinds that say what they are beside their name, because the
 *  group they sit in mixes kinds: an operation's transactions, a data
 *  model's outboxes, the runtime's declarations, the boundary vertices. */
const CAPTIONED: Partial<Record<PageKind, string>> = {
  transaction: "tx",
  outbox: "outbox",
  pool: "pool",
  router: "router",
  storage_layout: "storage",
  external: "external",
  clients: "clients",
};

/**
 * The model as a tree of pages: the topological hierarchy the canvas
 * shows one node of at a time. Services hold operations, operations
 * their transactions; data models hold objects and outboxes; state
 * machines and the runtime's declarations are listed flat.
 * The page in view is marked and its path kept open; a status dot on a
 * node is the worst verdict anchored to it or beneath it.
 */
export function Navigator() {
  const { model, graph, index, route, obligations, navigateTo } = useApp();
  const groups = useMemo(() => navigationTree(model, graph), [model, graph]);
  const path = useMemo(() => routePath(route, model, index), [route, model, index]);
  const current = path[path.length - 1];

  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => new Set(path));
  const [openGroups, setOpenGroups] = useState<ReadonlySet<string>>(
    () => new Set(groups.filter((g) => g.open).map((g) => g.key)),
  );
  const [query, setQuery] = useState("");

  // Whatever a reader has folded away, the path to the page in view is
  // open: the tree always shows where the reader is.
  useEffect(() => {
    setExpanded((prev) => {
      if (path.every((id) => prev.has(id))) return prev;
      const next = new Set(prev);
      for (const id of path) next.add(id);
      return next;
    });
    const group = groups.find((g) => g.nodes.some((n) => contains(n, current)));
    if (group) setOpenGroups((prev) => (prev.has(group.key) ? prev : new Set(prev).add(group.key)));
  }, [path, current, groups]);

  // The worst verdict at or beneath each node, so a folded service still
  // says whether anything inside it is unproven.
  const status = useMemo(() => {
    const map = new Map<string, Status | null>();
    const visit = (n: NavNode): Status | null => {
      let worst = worstStatus(n.obKeys.flatMap((key) => obligations.get(key) ?? []));
      for (const c of n.children) {
        const s = visit(c);
        if (s && (!worst || STATUS_ORDER[s] < STATUS_ORDER[worst])) worst = s;
      }
      map.set(pathId(n.kind, n.id), worst);
      return worst;
    };
    for (const g of groups) for (const n of g.nodes) visit(n);
    return map;
  }, [groups, obligations]);

  const q = query.trim().toLowerCase();
  const matches = (n: NavNode) => !q || n.id.toLowerCase().includes(q) || n.label.toLowerCase().includes(q);

  const toggle = (id: string) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  const toggleGroup = (key: string) =>
    setOpenGroups((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });

  const renderNode = (n: NavNode, depth: number): React.ReactNode => {
    const id = pathId(n.kind, n.id);
    // While filtering, a node stays only if it or something beneath it
    // matches, and every match's ancestors are open.
    const visibleChildren = q ? n.children.filter((c) => matches(c) || hasMatch(c, matches)) : n.children;
    if (q && !matches(n) && !visibleChildren.length) return null;
    const open = q ? true : expanded.has(id);
    const active = id === current;
    const onPath = path.includes(id);
    const s = status.get(id) ?? null;
    const caption = CAPTIONED[n.kind];
    return (
      <li key={id}>
        <div
          className={`flex items-center gap-0.5 rounded-md pr-1.5 ${active ? "bg-kumo-brand/10" : "hover:bg-kumo-tint"}`}
          style={{ paddingLeft: 4 + depth * 14 }}
        >
          {n.children.length ? (
            <button
              type="button"
              className="flex h-6 w-5 shrink-0 cursor-pointer items-center justify-center rounded text-kumo-inactive hover:text-kumo-default"
              aria-label={open ? `Collapse ${n.label}` : `Expand ${n.label}`}
              aria-expanded={open}
              onClick={() => toggle(id)}
            >
              <CaretRightIcon size={11} className={`transition-transform ${open ? "rotate-90" : ""}`} />
            </button>
          ) : (
            <span className="w-5 shrink-0" />
          )}
          <button
            type="button"
            className={`flex min-w-0 flex-1 cursor-pointer items-center gap-1.5 py-1 text-left ${active ? "text-kumo-strong" : onPath ? "text-kumo-default" : "text-kumo-default"}`}
            title={`${n.id}\n${PAGE_KIND_LABEL[n.kind]}`}
            aria-current={active ? "page" : undefined}
            onClick={() => navigateTo(n.hash)}
          >
            <span className={`truncate font-mono text-[12px] ${active ? "font-semibold" : ""}`}>{n.label}</span>
            {caption && (
              <span className="shrink-0 text-[9.5px] font-semibold uppercase tracking-wider text-kumo-inactive">{caption}</span>
            )}
          </button>
          {s && (
            <span
              className="h-2 w-2 shrink-0 rounded-full"
              style={{ background: `var(--arch-${s})` }}
              title={`worst verdict here or beneath: ${s}`}
            />
          )}
        </div>
        {open && visibleChildren.length > 0 && <ul>{visibleChildren.map((c) => renderNode(c, depth + 1))}</ul>}
      </li>
    );
  };

  const renderGroup = (g: NavGroup) => {
    const open = q ? true : openGroups.has(g.key);
    const nodes = q ? g.nodes.filter((n) => matches(n) || hasMatch(n, matches)) : g.nodes;
    if (q && !nodes.length) return null;
    return (
      <li key={g.key} className="space-y-0.5">
        <button
          type="button"
          className="flex w-full cursor-pointer items-center gap-1 rounded-md px-1 py-1 text-left hover:bg-kumo-tint"
          aria-expanded={open}
          onClick={() => toggleGroup(g.key)}
        >
          <CaretRightIcon size={11} className={`shrink-0 text-kumo-inactive transition-transform ${open ? "rotate-90" : ""}`} />
          <span className="text-[11px] font-semibold uppercase tracking-wider text-kumo-subtle">{g.title}</span>
          <span className="ml-auto text-[11px] text-kumo-inactive">{countLeaves(g.nodes)}</span>
        </button>
        {open && <ul>{nodes.map((n) => renderNode(n, 0))}</ul>}
      </li>
    );
  };

  const systemActive = route.view === "system";

  return (
    <nav className="flex h-full flex-col" aria-label="Model navigator">
      {/* The filter sits in a row the height of the top bar, so the rule
          under it runs straight across into the bar and the panels. */}
      <div className="box-content flex h-12 shrink-0 items-center border-b border-kumo-hairline px-3">
        <Input
          size="sm"
          className="w-full"
          placeholder="filter…"
          aria-label="Filter the navigator"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto px-2 py-2">
        <button
          type="button"
          className={`mb-1 flex w-full cursor-pointer items-center gap-2 rounded-md px-2 py-1.5 text-left ${systemActive ? "bg-kumo-brand/10 text-kumo-strong" : "hover:bg-kumo-tint"}`}
          aria-current={systemActive ? "page" : undefined}
          onClick={() => navigateTo(hashes.system())}
        >
          <GraphIcon size={14} className="shrink-0 text-kumo-subtle" />
          <span className={`text-[12.5px] ${systemActive ? "font-semibold" : ""}`}>system</span>
          <span className="ml-auto text-[10px] uppercase tracking-wider text-kumo-inactive">graph</span>
        </button>
        <ul className="space-y-1">{groups.map(renderGroup)}</ul>
      </div>
    </nav>
  );
}

function contains(n: NavNode, id: string): boolean {
  return pathId(n.kind, n.id) === id || n.children.some((c) => contains(c, id));
}

function hasMatch(n: NavNode, matches: (n: NavNode) => boolean): boolean {
  return n.children.some((c) => matches(c) || hasMatch(c, matches));
}

function countLeaves(nodes: NavNode[]): number {
  return nodes.reduce((sum, n) => sum + (n.children.length ? countLeaves(n.children) : 1), 0);
}
