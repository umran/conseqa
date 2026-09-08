import { Badge } from "@cloudflare/kumo/components/badge";
import { Button } from "@cloudflare/kumo/components/button";
import { Collapsible } from "@cloudflare/kumo/components/collapsible";
import { Empty } from "@cloudflare/kumo/components/empty";
import { Input } from "@cloudflare/kumo/components/input";
import { Tabs } from "@cloudflare/kumo/components/tabs";
import { Text } from "@cloudflare/kumo/components/text";
import { CaretRightIcon, ListChecksIcon, WarningIcon, XIcon } from "@phosphor-icons/react";
import { useMemo, useState } from "react";

import { shortId } from "../lib/ids";
import {
  STATUS_GLYPH, STATUS_ORDER, obligationLayer, statusCounts, subjectGroup, subjectText, worstStatus,
  type Layer,
} from "../lib/obligations";
import { useApp } from "../state/AppState";
import { propertyName, type EvidenceItem, type Obligation, type Status } from "../types/report";
import { ObligationCard } from "./ObligationCard";
import { StatusBadge } from "./parts";

type Filter = "all" | Status;
type LayerFilter = "any" | Layer;

export function ObligationsPanel() {
  const { report, setObligationsOpen } = useApp();
  const [filter, setFilter] = useState<Filter>("all");
  const [layer, setLayer] = useState<LayerFilter>("any");
  const [query, setQuery] = useState("");

  const all = report?.obligations ?? [];
  const counts = statusCounts(all);
  const layerCounts = { l0: 0, runtime: 0 };
  for (const ob of all) {
    const l = obligationLayer(ob);
    if (l) layerCounts[l]++;
  }

  const groups = useMemo(() => {
    const q = query.trim().toLowerCase();
    const visible = all.filter(
      (ob) =>
        (filter === "all" || ob.status === filter) &&
        (layer === "any" || obligationLayer(ob) === layer) &&
        (!q ||
          ob.id.toLowerCase().includes(q) ||
          ob.summary.toLowerCase().includes(q) ||
          subjectText(ob.subject).toLowerCase().includes(q) ||
          propertyName(ob.property).includes(q)),
    );
    const byGroup = new Map<string, Obligation[]>();
    for (const ob of visible) {
      const g = subjectGroup(ob.subject);
      const list = byGroup.get(g);
      if (list) list.push(ob);
      else byGroup.set(g, [ob]);
    }
    return [...byGroup.entries()]
      .map(([id, obs]) => ({
        id,
        obs: [...obs].sort((a, b) => STATUS_ORDER[a.status] - STATUS_ORDER[b.status] || a.id.localeCompare(b.id)),
      }))
      .sort((a, b) => STATUS_ORDER[worstStatus(a.obs)!] - STATUS_ORDER[worstStatus(b.obs)!] || a.id.localeCompare(b.id));
  }, [all, filter, layer, query]);

  if (!report) return null;

  const tabs = [
    { value: "all", label: `all ${all.length}` },
    { value: "unknown", label: `${STATUS_GLYPH.unknown} ${counts.unknown ?? 0}` },
    { value: "proven", label: `${STATUS_GLYPH.proven} ${counts.proven ?? 0}` },
  ];
  if (counts.disproven) tabs.splice(1, 0, { value: "disproven", label: `${STATUS_GLYPH.disproven} ${counts.disproven}` });

  // The second question a reader has about a verdict, after whether it
  // holds: which layer it turns on. For a proof that is the layers it
  // consumed; for an unproven one, the layer its missing facts belong to.
  const layerTabs = [
    { value: "any", label: "any layer" },
    { value: "l0", label: `L0 ${layerCounts.l0}` },
    { value: "runtime", label: `L1 ${layerCounts.runtime}` },
  ];

  return (
    <div className="flex h-full flex-col">
      <header className="flex shrink-0 items-center justify-between border-b border-kumo-hairline px-4 py-2">
        <span className="flex items-center gap-2">
          <ListChecksIcon size={16} className="text-kumo-subtle" />
          <span className="uppercase tracking-wider">
            <Text variant="secondary" size="xs" as="span">
              obligations
            </Text>
          </span>
        </span>
        <Button variant="ghost" size="xs" shape="square" icon={XIcon} aria-label="Close" onClick={() => setObligationsOpen(false)} />
      </header>
      <div className="shrink-0 space-y-2 border-b border-kumo-hairline px-4 py-2.5">
        <Tabs variant="segmented" size="sm" tabs={tabs} value={filter} onValueChange={(v) => setFilter(v as Filter)} />
        <Tabs
          variant="segmented"
          size="sm"
          tabs={layerTabs}
          value={layer}
          onValueChange={(v) => setLayer(v as LayerFilter)}
        />
        <Input size="sm" placeholder="filter obligations…" value={query} onChange={(e) => setQuery(e.target.value)} />
      </div>
      <div className="flex-1 space-y-3 overflow-y-auto px-4 py-3">
        {filter === "all" && layer === "any" && !query && (report.notes?.length ?? 0) > 0 && (
          <NotesGroup notes={report.notes ?? []} />
        )}
        {groups.length === 0 && (
          <Empty size="sm" title="no obligations match" description="Adjust the status filter, the layer filter, or the search." />
        )}
        {groups.map((group) => (
          <ObligationGroup key={group.id} id={group.id} obs={group.obs} />
        ))}
      </div>
    </div>
  );
}

/** Model-wide warnings: gaps no obligation covers, raised by the checker. */
function NotesGroup({ notes }: { notes: EvidenceItem[] }) {
  const { openDetail } = useApp();
  const [open, setOpen] = useState(true);
  return (
    <Collapsible.Root open={open} onOpenChange={setOpen}>
      <Collapsible.Trigger className="flex w-full cursor-pointer items-center justify-between gap-2 rounded-md px-1 py-1 text-left hover:bg-kumo-tint">
        <span className="flex min-w-0 items-center gap-1.5">
          <CaretRightIcon size={12} className={`shrink-0 text-kumo-inactive transition-transform ${open ? "rotate-90" : ""}`} />
          <WarningIcon size={14} className="shrink-0 text-kumo-warning" />
          <span className="truncate text-[12px] font-semibold text-kumo-strong">notes</span>
        </span>
        <Badge variant="warning" appearance="dot">{notes.length}</Badge>
      </Collapsible.Trigger>
      <Collapsible.Panel>
        <ul className="space-y-2 pl-1 pt-2">
          {notes.map((note, i) => (
            <li key={i} className="rounded-md border border-kumo-warning/40 bg-kumo-warning-tint/40 p-2.5 text-xs leading-relaxed text-kumo-default">
              {note.subject && (
                <button type="button" className="mb-1 block cursor-pointer font-mono text-[11px] text-kumo-link hover:underline" onClick={() => openDetail(note.subject!)}>
                  {shortId(note.subject)}
                </button>
              )}
              {note.message}
            </li>
          ))}
        </ul>
      </Collapsible.Panel>
    </Collapsible.Root>
  );
}

function ObligationGroup({ id, obs }: { id: string; obs: Obligation[] }) {
  const [open, setOpen] = useState(true);
  const counts = statusCounts(obs);
  return (
    <Collapsible.Root open={open} onOpenChange={setOpen}>
      <Collapsible.Trigger className="flex w-full cursor-pointer items-center justify-between gap-2 rounded-md px-1 py-1 text-left hover:bg-kumo-tint">
        <span className="flex min-w-0 items-center gap-1.5">
          <CaretRightIcon size={12} className={`shrink-0 text-kumo-inactive transition-transform ${open ? "rotate-90" : ""}`} />
          <span className="truncate font-mono text-[12px] font-semibold text-kumo-strong">{shortId(id)}</span>
        </span>
        <span className="flex shrink-0 items-center gap-1">
          {(["disproven", "unknown", "proven"] as const).map((s) =>
            counts[s] ? (
              <Badge key={s} variant={s === "proven" ? "success" : s === "disproven" ? "error" : "warning"} appearance="dot">
                {counts[s]}
              </Badge>
            ) : null,
          )}
        </span>
      </Collapsible.Trigger>
      <Collapsible.Panel>
        <div className="space-y-2 pl-1 pt-2">
          {obs.map((ob) => (
            <ObligationCard key={ob.id} ob={ob} />
          ))}
        </div>
      </Collapsible.Panel>
    </Collapsible.Root>
  );
}

export function ObligationsSummaryBadge() {
  const { report } = useApp();
  if (!report) return null;
  const counts = statusCounts(report.obligations);
  return (
    <span className="flex items-center gap-1">
      {(["disproven", "unknown", "proven"] as const).map((s) =>
        counts[s] ? <StatusBadge key={s} status={s} /> : null,
      )}
    </span>
  );
}
