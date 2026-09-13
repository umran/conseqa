import { Badge } from "@cloudflare/kumo/components/badge";
import { Button } from "@cloudflare/kumo/components/button";
import { Collapsible } from "@cloudflare/kumo/components/collapsible";
import { Tooltip } from "@cloudflare/kumo/components/tooltip";
import { CaretDownIcon, CaretRightIcon } from "@phosphor-icons/react";
import { useEffect, useId, useMemo, useRef, useState, type ReactNode } from "react";

import { ConsistencyGraph } from "../graph/ConsistencyGraph";
import { cursorRule, fence, serializabilityRoute, type Explanation } from "../lib/explain";
import { shortId } from "../lib/ids";
import type { DependencyKind, DependencyView, OrderingView, PairView, SerializabilityView } from "../types/consistency";
import { CitedText, FactBadge, FactNote, Mono, Section, StatusBadge } from "./parts";

// The two transaction consistency arguments, drawn. A serializability
// verdict is an argument over the conflict closure; an ordering verdict
// is that argument plus one guard step. Everything here is read off the
// argument the page data carries — the checker's own — and nothing
// here consults runtime topology, because no such proof does.

const KIND_TONE: Record<DependencyKind, "info" | "orange" | "purple"> = {
  wr: "info",
  rw: "orange",
  ww: "purple",
};

function Heading({ children }: { children: ReactNode }) {
  return <div className="mb-1 text-[11px] font-semibold uppercase tracking-wider text-kumo-subtle">{children}</div>;
}

function AmberNote({ children }: { children: ReactNode }) {
  return (
    <div className="rounded-md border border-kumo-warning/40 bg-kumo-warning-tint/40 px-2.5 py-1.5 text-xs leading-relaxed text-kumo-default">
      {children}
    </div>
  );
}

/** The checker's obstacle sentences, one warning row each. */
function Obstacles({ items }: { items: string[] }) {
  if (!items.length) return null;
  return (
    <div>
      <Heading>obstacles</Heading>
      <ul className="space-y-1.5">
        {items.map((text, i) => (
          <li key={i}>
            <AmberNote>
              <CitedText text={text} />
            </AmberNote>
          </li>
        ))}
      </ul>
    </div>
  );
}

function RevealToggle({ open, onToggle }: { open: boolean; onToggle: () => void }) {
  return (
    <Button variant="ghost" size="xs" icon={open ? CaretDownIcon : CaretRightIcon} onClick={onToggle}>
      {open ? "hide the argument" : "show the argument"}
    </Button>
  );
}

// ---------------------------------------------------------------------------
// Serializability
// ---------------------------------------------------------------------------

/**
 * `SerializableBy(key)`, argued: the status and the route, the
 * headline, the closure drawn, and — expanded — the dependencies each
 * arrow stands for and the obstacles when the argument fails. Compact
 * shows the headline and the drawing, with the rest behind a toggle.
 */
export function SerializabilityProof({ view, compact = false }: { view: SerializabilityView; compact?: boolean }) {
  const [selected, setSelected] = useState<string | null>(null);
  const [expanded, setExpanded] = useState(!compact);
  const select = (id: string | null) => {
    setSelected(id);
    if (id) setExpanded(true);
  };

  return (
    <div className="space-y-3">
      <div className="space-y-1.5">
        <div className="flex flex-wrap items-center gap-1.5">
          <StatusBadge status={view.proven ? "proven" : "unknown"} />
          <FactBadge fact={serializabilityRoute(view.route)} />
          <span className="text-xs text-kumo-inactive">
            closure of {view.nodes.length} · {view.edges.length} dependenc{view.edges.length === 1 ? "y" : "ies"}
          </span>
        </div>
        <p className="text-sm leading-relaxed text-kumo-default">
          <CitedText text={view.headline} />
        </p>
      </div>
      <ConsistencyGraph view={view} selectedPair={selected} onSelectPair={select} />
      {compact && <RevealToggle open={expanded} onToggle={() => setExpanded((v) => !v)} />}
      {expanded && (
        <>
          <Dependencies view={view} selected={selected} onSelect={select} />
          {!view.proven && <Obstacles items={view.obstacles} />}
        </>
      )}
    </div>
  );
}

/** The dependencies, grouped by the arrow they belong to. The group of
 *  the arrow selected in the drawing opens and scrolls into view. Under
 *  the isolation route the section starts closed: that route never
 *  consults them. */
function Dependencies({
  view, selected, onSelect,
}: { view: SerializabilityView; selected: string | null; onSelect: (id: string) => void }) {
  const isolationRoute = view.route === "serializable_isolation";
  const [open, setOpen] = useState(!isolationRoute);
  useEffect(() => {
    if (selected) setOpen(true);
  }, [selected]);
  const byId = useMemo(() => new Map(view.edges.map((e) => [e.id, e])), [view]);

  return (
    <Section title="dependencies" count={view.edges.length} open={open} onOpenChange={setOpen}>
      <p className="text-xs leading-relaxed text-kumo-subtle">
        {isolationRoute
          ? "The isolation route does not consult them; shown for reference. Each is a potential dependency between two steps that could close a cycle, with the fact that would commit-order it under the graph route."
          : "Each is a potential dependency between two steps that could close a cycle, and the declared fact that commit-orders it — or what is missing. An open row is the argument's gap."}
      </p>
      {view.pairs.map((pair) => (
        <PairGroup
          key={pair.id}
          pair={pair}
          deps={pair.edge_ids.map((id) => byId.get(id)).filter((d): d is DependencyView => !!d)}
          selected={selected === pair.id}
          onSelect={() => onSelect(pair.id)}
        />
      ))}
    </Section>
  );
}

function PairGroup({
  pair, deps, selected, onSelect,
}: { pair: PairView; deps: DependencyView[]; selected: boolean; onSelect: () => void }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!selected) return;
    setOpen(true);
    ref.current?.scrollIntoView({ block: "nearest", behavior: "smooth" });
  }, [selected]);

  const stripe = pair.constrained ? "border-l-kumo-success" : "border-l-kumo-warning";
  return (
    <Collapsible.Root open={open} onOpenChange={setOpen}>
      <div
        ref={ref}
        className={`rounded-md border border-l-2 border-kumo-hairline bg-kumo-elevated/40 ${stripe} ${selected ? "ring-1 ring-kumo-brand" : ""}`}
      >
        <Collapsible.Trigger
          className="flex w-full cursor-pointer items-center justify-between gap-2 px-2.5 py-1.5 text-left"
          onClick={onSelect}
        >
          <span className="flex min-w-0 items-center gap-1.5">
            <CaretRightIcon size={12} className={`shrink-0 text-kumo-inactive transition-transform ${open ? "rotate-90" : ""}`} />
            <Mono className="truncate text-kumo-strong">
              {shortId(pair.source)} → {shortId(pair.target)}
            </Mono>
            {pair.self_loop && <Badge variant="neutral">concurrent self</Badge>}
          </span>
          <span className="flex shrink-0 items-center gap-1.5">
            <span className="text-xs text-kumo-inactive">
              {pair.dependencies} dep{pair.dependencies === 1 ? "" : "s"}
            </span>
            {pair.constrained ? (
              <Badge variant="success" appearance="dot">proven</Badge>
            ) : (
              <Badge variant="warning" appearance="dot">{pair.open} open</Badge>
            )}
          </span>
        </Collapsible.Trigger>
        <Collapsible.Panel>
          <ul className="space-y-1.5 border-t border-kumo-hairline px-2.5 py-2">
            {deps.map((dep) => (
              <DependencyRow key={dep.id} dep={dep} />
            ))}
          </ul>
        </Collapsible.Panel>
      </div>
    </Collapsible.Root>
  );
}

/** One potential dependency: the two accesses, its kind, the object and
 *  fields, the overlap, and the evidence that orders it or the gaps
 *  that do not, with the checker's sentence beneath. */
function DependencyRow({ dep }: { dep: DependencyView }) {
  // `fields` is a single field, a list, or `all fields` / `unknown
  // fields`; only the single field reads as a member of the object.
  const single = /^[A-Za-z0-9_.]+$/.test(dep.fields);
  return (
    <li className="rounded bg-kumo-base px-2 py-1.5 text-xs">
      <div className="flex flex-wrap items-center gap-x-1.5 gap-y-1">
        <Tooltip
          content={dep.kind_label}
          render={
            <span className="inline-flex">
              <Badge variant={KIND_TONE[dep.kind]}>{dep.kind}</Badge>
            </span>
          }
        />
        <Mono className="text-kumo-strong">step {dep.source_step}</Mono>
        <span className="text-kumo-subtle">({dep.source_mode})</span>
        <span className="text-kumo-inactive">→</span>
        <Mono className="text-kumo-strong">step {dep.target_step}</Mono>
        <span className="text-kumo-subtle">({dep.target_mode})</span>
        <Mono className="text-kumo-subtle">
          {single ? `${shortId(dep.object)}.${dep.fields}` : `${shortId(dep.object)} · ${dep.fields}`}
        </Mono>
        <Badge variant="outline">{dep.overlap}</Badge>
        {dep.constrained ? (
          <Badge variant="success">{dep.evidence ?? "commit-ordered"}</Badge>
        ) : (
          dep.gaps.map((gap) => (
            <Badge key={gap} variant="warning">{gap}</Badge>
          ))
        )}
        {dep.fence && (
          <Tooltip
            content="A fence is recorded on this edge. A fence orders authority generations; it is never serializability evidence on its own."
            render={
              <span className="inline-flex">
                <Badge variant="neutral">fence recorded</Badge>
              </span>
            }
          />
        )}
      </div>
      <p className="mt-1 leading-relaxed text-kumo-subtle">
        <CitedText text={dep.explanation} />
      </p>
    </li>
  );
}

// ---------------------------------------------------------------------------
// Ordering
// ---------------------------------------------------------------------------

function guardFact(view: OrderingView): Explanation {
  const m = view.mechanism;
  if (!m) {
    return {
      label: "no guard",
      tone: "warning",
      summary:
        "No advance_cursor or fence step of the transaction carries the position on a managed " +
        "field of the keyed object, so nothing makes commits follow it. Transport precedence is " +
        "never a route.",
    };
  }
  if (m.kind === "fence") return fence();
  if (m.rule === "successor" || m.rule === "monotonic_after") return cursorRule(m.rule);
  return { label: "cursor", tone: "info", summary: "An advance_cursor step admits the incoming position after the stored one under its rule." };
}

/**
 * `OrderedBy(key, position)`, argued: the guard that carries the
 * position onto a managed field of the keyed object, drawn as a strip,
 * over the serializability argument the ordering presupposes. Compact
 * shows the headline and the strip, with the rest behind a toggle.
 */
export function OrderingProof({ view, compact = false }: { view: OrderingView; compact?: boolean }) {
  const [expanded, setExpanded] = useState(!compact);
  const m = view.mechanism;
  const guard = guardFact(view);
  const routeLabel = m ? (m.kind === "cursor" ? "cursor" : "fence") : "no guard";

  return (
    <div className="space-y-3">
      <div className="space-y-1.5">
        <div className="flex flex-wrap items-center gap-1.5">
          <StatusBadge status={view.proven ? "proven" : "unknown"} />
          <Tooltip
            content={guard.summary}
            render={
              <span className="inline-flex">
                <Badge variant={m ? "info" : "warning"}>{routeLabel}</Badge>
              </span>
            }
          />
        </div>
        <p className="text-sm leading-relaxed text-kumo-default">
          <CitedText text={view.headline} />
        </p>
      </div>
      <MechanismStrip view={view} guard={guard} />
      {compact && <RevealToggle open={expanded} onToggle={() => setExpanded((v) => !v)} />}
      {expanded && (
        <>
          <div className="text-xs leading-relaxed text-kumo-subtle">
            within each <Mono className="text-kumo-default"><CitedText text={view.key} /></Mono>, committed executions take
            effect in the order of <Mono className="text-kumo-default"><CitedText text={view.position} /></Mono>
          </div>
          <div className="space-y-2 rounded-md border border-kumo-hairline p-2.5">
            <Heading>rests on serializability over that key</Heading>
            <SerializabilityProof view={view.serializability} compact={view.serializability.proven} />
          </div>
          {!view.proven && <Obstacles items={view.obstacles} />}
        </>
      )}
    </div>
  );
}

/** The guard as a picture: the position, the rule that admits it, the
 *  managed field it lands on; and beneath, what that rule means. */
function MechanismStrip({ view, guard }: { view: OrderingView; guard: Explanation }) {
  const m = view.mechanism;
  const rule = m ? (m.kind === "cursor" ? (m.rule ?? "cursor") : "fence") : "no guard";
  const broken = !m || !m.carries_position;
  return (
    <div className="arch-mechanism space-y-2 rounded-md border border-kumo-hairline bg-kumo-elevated/40 p-2.5">
      <div className="flex items-stretch gap-2">
        <Chip caption="position">
          <CitedText text={view.position} />
        </Chip>
        <Wire label={rule} broken={broken} />
        <Chip caption={m ? (m.kind === "cursor" ? "cursor field" : "fence field") : "managed field"} warning={!m}>
          {m ? `${shortId(m.object)}.${m.field}` : "none"}
        </Chip>
      </div>
      <div className="text-center text-[11px] text-kumo-subtle">
        {m ? `${m.kind === "cursor" ? "advance_cursor" : "fence"} at step ${m.step}` : "no cursor or fence carries the position"}
      </div>
      {!m && <AmberNote>No cursor or fence carries the position: the transaction has no advance_cursor or fence step on a managed field of the keyed object whose incoming value is the position.</AmberNote>}
      {m && !m.carries_position && (
        <AmberNote>
          The guard's incoming value <Mono>{m.incoming}</Mono> is not the position <Mono>{view.position}</Mono>, so
          what it orders is not what the requirement asks to be ordered.
        </AmberNote>
      )}
      {m && <FactNote fact={guard} />}
    </div>
  );
}

function Chip({ caption, warning, children }: { caption: string; warning?: boolean; children: ReactNode }) {
  return (
    <div className={`min-w-0 flex-1 rounded-md border bg-kumo-base px-2 py-1 ${warning ? "border-kumo-warning/60" : "border-kumo-hairline"}`}>
      <div className="text-[9.5px] font-semibold uppercase tracking-wider text-kumo-inactive">{caption}</div>
      <Mono className="break-all text-kumo-strong">{children}</Mono>
    </div>
  );
}

/** The connector between the two chips: a line carrying the rule, with
 *  an arrowhead into the field. Dashed and amber when nothing carries
 *  the position across it. */
function Wire({ label, broken }: { label: string; broken: boolean }) {
  const id = `mech-${useId().replace(/[^A-Za-z0-9_-]/g, "")}`;
  const color = broken ? "var(--arch-unknown)" : "var(--arch-proven)";
  return (
    <svg className="w-24 shrink-0 self-center" height={34} aria-hidden>
      <defs>
        <marker id={id} markerWidth={9} markerHeight={7} refX={8} refY={3.5} orient="auto" markerUnits="userSpaceOnUse">
          <path d="M0,0 L9,3.5 L0,7 Z" fill={color} />
        </marker>
      </defs>
      <text className="rule" x="50%" y={12} textAnchor="middle">{label}</text>
      <line className={`wire${broken ? " broken" : ""}`} x1={0} y1={24} x2="100%" y2={24} markerEnd={`url(#${id})`} />
    </svg>
  );
}
