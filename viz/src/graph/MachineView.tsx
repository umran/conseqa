import { Badge } from "@cloudflare/kumo/components/badge";
import { ClipboardText } from "@cloudflare/kumo/components/clipboard-text";
import { Empty } from "@cloudflare/kumo/components/empty";
import { Table } from "@cloudflare/kumo/components/table";
import { Text } from "@cloudflare/kumo/components/text";
import { GraphIcon } from "@phosphor-icons/react";
import { useMemo } from "react";

import { pathText, shortId } from "../lib/ids";
import { intentExecutors } from "../lib/index";
import { worstStatus } from "../lib/obligations";
import { hashes } from "../lib/route";
import { useApp } from "../state/AppState";
import { Fact, IdLink, Mono, SectionCard, StatusChips, selectableRow } from "../panels/parts";
import type { Id, TransitionSideEffect } from "../types/model";
import { layoutMachine } from "./layoutMachine";

const PAD = 56;
const LABEL_W = 150;

/** One transition-owned side effect: what it is, where it goes, and which
 *  operations execute it through an intent the transition establishes. */
function SideEffectItem({ effectId, effect }: { effectId: Id; effect: TransitionSideEffect }) {
  const { model } = useApp();
  const executors = intentExecutors(model, effectId);
  return (
    <li className="flex items-start gap-2">
      <Badge variant={effect.kind === "publication" ? "purple" : "orange"}>{effect.kind}</Badge>
      <div className="min-w-0 space-y-0.5">
        <div>
          <IdLink id={effectId}>{shortId(effectId)}</IdLink>
        </div>
        <div className="flex flex-wrap items-center gap-x-1.5 text-xs text-kumo-subtle">
          {effect.kind === "publication" ? (
            <>
              <span>→ topic</span>
              <IdLink id={effect.topic}>{shortId(effect.topic)}</IdLink>
              <span>· schema</span>
              <IdLink id={effect.schema}>{shortId(effect.schema)}</IdLink>
            </>
          ) : (
            <>
              <span>→ operation</span>
              <IdLink id={effect.target.operation}>{shortId(effect.target.operation)}</IdLink>
              <span>input</span>
              <IdLink id={effect.target.input}>{shortId(effect.target.input)}</IdLink>
              <span>· retry {effect.retry}</span>
            </>
          )}
        </div>
        <div className="flex flex-wrap items-center gap-x-1.5 text-xs text-kumo-subtle">
          {executors.length ? (
            <>
              <span>executed by</span>
              {executors.map((x) => (
                <IdLink key={x.intent} id={x.op}>{shortId(x.op)}</IdLink>
              ))}
            </>
          ) : (
            <span className="text-kumo-inactive">no operation executes it</span>
          )}
        </div>
      </div>
    </li>
  );
}

export function MachineView({ id, highlight }: { id: string; highlight: string | null }) {
  const { model, graph, selection, obligations, select, navigateTo } = useApp();
  const machine = model.state_machines[id];
  const layout = useMemo(() => (machine ? layoutMachine(machine) : null), [machine]);

  if (!machine || !layout) {
    return (
      <div className="flex h-full items-center justify-center">
        <Empty size="sm" icon={<GraphIcon size={32} className="text-kumo-inactive" />} title={`unknown state machine ${id}`} />
      </div>
    );
  }

  // The address bar names the selected transition, so selecting one here
  // navigates to it, and selecting a state drops it. The route then
  // implies the selection, which keeps clicks, deep links and history
  // navigation in agreement.
  const chooseTransition = (tId: Id) => {
    select(`t:${tId}`, { id: tId, ctx: {} });
    navigateTo(hashes.machine(id, tId), `t:${tId}`);
  };
  const chooseState = (s: Id) => {
    select(`s:${s}`, { id: s, ctx: {} });
    navigateTo(hashes.machine(id), `s:${s}`);
  };

  // Scene bounds over nodes and labels, translated into a positive frame.
  const xs: number[] = [];
  const ys: number[] = [];
  for (const p of layout.pos.values()) {
    xs.push(p.x, p.x + p.w);
    ys.push(p.y, p.y + p.h);
  }
  for (const e of layout.edges) {
    xs.push(e.labelX, e.labelX + LABEL_W);
    ys.push(e.labelY - 12, e.labelY + 12);
  }
  const minX = Math.min(0, ...xs) - PAD;
  const minY = Math.min(0, ...ys) - PAD;
  const width = Math.max(...xs) - minX + PAD;
  const height = Math.max(...ys) - minY + PAD;
  const tx = (x: number) => x - minX;
  const ty = (y: number) => y - minY;

  const transitions = Object.entries(machine.transitions);
  const sideEffectCount = transitions.reduce((n, [, t]) => n + Object.keys(t.side_effects).length, 0);

  return (
    <div className="h-full overflow-auto">
      <div className="mx-auto max-w-[1240px] space-y-6 p-6">
        <header className="space-y-4 border-b border-kumo-hairline pb-5">
          <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
            <Text variant="heading" size="lg" as="h1">{shortId(id)}</Text>
            <ClipboardText text={id} size="sm" tooltip={{ text: "Copy id", copiedText: "Copied" }} />
          </div>
          <dl className="flex flex-wrap gap-x-8 gap-y-3">
            <Fact label="governs"><IdLink id={machine.subject.object}>{shortId(machine.subject.object)}</IdLink></Fact>
            <Fact label="state field"><Mono>{pathText(machine.subject.state)}</Mono></Fact>
            <Fact label="initial state"><Mono>{shortId(machine.initial)}</Mono></Fact>
            <Fact label="states">{machine.states.length}</Fact>
            <Fact label="transitions">{transitions.length}</Fact>
            <Fact label="side effects">{sideEffectCount}</Fact>
            {(obligations.get(id) ?? []).length > 0 && <Fact label="verdicts"><StatusChips obKey={id} /></Fact>}
          </dl>
        </header>

        <SectionCard title="State graph" hint="legal states and the transitions between them; ⚡ counts a transition's side effects" bodyClassName="p-4">
          <div className="overflow-x-auto">
            <div className="relative rounded-lg border border-kumo-hairline bg-kumo-elevated/30" style={{ width, height }}>
              <svg className="absolute inset-0" width={width} height={height} style={{ pointerEvents: "none" }}>
                <defs>
                  <marker id="sm-arrow" markerWidth={9} markerHeight={7} refX={8} refY={3.5} orient="auto" markerUnits="userSpaceOnUse">
                    <path d="M0,0 L9,3.5 L0,7 Z" fill="var(--arch-text-subtle)" />
                  </marker>
                  <marker id="sm-arrow-accent" markerWidth={9} markerHeight={7} refX={8} refY={3.5} orient="auto" markerUnits="userSpaceOnUse">
                    <path d="M0,0 L9,3.5 L0,7 Z" fill="var(--arch-accent)" />
                  </marker>
                </defs>
                <g transform={`translate(${-minX},${-minY})`}>
                  {layout.edges.map((edge) => {
                    const key = `t:${edge.transition}`;
                    const active = selection === key || highlight === edge.transition;
                    const obs = obligations.get(`${id}/${edge.transition}`) ?? [];
                    const status = obs.length ? worstStatus(obs) : null;
                    const stroke = active ? "var(--arch-accent)" : status ? `var(--arch-${status})` : undefined;
                    return (
                      <g key={`${edge.transition}|${edge.from}`}>
                        <path
                          className={`arch-sm-edge${active ? " selected" : ""}`}
                          d={edge.d}
                          style={stroke ? { stroke } : undefined}
                          markerEnd={active ? "url(#sm-arrow-accent)" : "url(#sm-arrow)"}
                        />
                        <path
                          className="arch-edge-hit"
                          d={edge.d}
                          style={{ pointerEvents: "stroke" }}
                          onClick={() => chooseTransition(edge.transition)}
                        />
                      </g>
                    );
                  })}
                </g>
              </svg>

              {machine.states.map((s) => {
                const p = layout.pos.get(s)!;
                const key = `s:${s}`;
                const selected = selection === key;
                const initial = s === machine.initial;
                return (
                  <button
                    key={s}
                    type="button"
                    onClick={() => chooseState(s)}
                    className={`absolute flex cursor-pointer flex-col items-center justify-center gap-1 rounded-xl border bg-kumo-base shadow-sm transition-shadow hover:shadow-md ${
                      initial ? "border-kumo-success" : "border-kumo-line"
                    } ${selected ? "ring-2 ring-kumo-brand" : ""}`}
                    style={{ left: tx(p.x), top: ty(p.y), width: p.w, height: p.h }}
                  >
                    <span className="font-mono text-[13px] font-semibold text-kumo-strong">{p.label}</span>
                    {initial && <Badge variant="success" appearance="dot">initial</Badge>}
                  </button>
                );
              })}

              {layout.edges.map((edge) => {
                const t = machine.transitions[edge.transition];
                const key = `t:${edge.transition}`;
                const active = selection === key || highlight === edge.transition;
                const fxCount = Object.keys(t.side_effects).length;
                return (
                  <button
                    key={`label:${edge.transition}|${edge.from}`}
                    type="button"
                    onClick={() => chooseTransition(edge.transition)}
                    className={`absolute -translate-y-1/2 cursor-pointer rounded-full ${active ? "ring-2 ring-kumo-brand" : ""}`}
                    style={{ left: tx(edge.labelX), top: ty(edge.labelY) }}
                    title={`${edge.transition}: ${edge.from} → ${t.to}`}
                  >
                    <Badge variant={active ? "info" : "outline"}>
                      {shortId(edge.transition)}
                      {fxCount ? ` ⚡${fxCount}` : ""}
                    </Badge>
                  </button>
                );
              })}
            </div>
          </div>
        </SectionCard>

        <SectionCard
          title="Transitions"
          count={transitions.length}
          hint="each with its side effects and the transaction steps that take it"
        >
          <div className="overflow-x-auto">
            <Table>
              <Table.Header variant="compact">
                <Table.Row>
                  <Table.Head>transition</Table.Head>
                  <Table.Head>from</Table.Head>
                  <Table.Head>to</Table.Head>
                  <Table.Head>side effects</Table.Head>
                  <Table.Head>taken by</Table.Head>
                  <Table.Head>verdicts</Table.Head>
                </Table.Row>
              </Table.Header>
              <Table.Body>
                {transitions.map(([tId, t]) => {
                  const key = `t:${tId}`;
                  const refs = graph.transition_refs[`${id}/${tId}`] ?? [];
                  const effects = Object.entries(t.side_effects);
                  return (
                    <Table.Row key={tId} className={selectableRow(selection === key)} onClick={() => chooseTransition(tId)}>
                      <Table.Cell className="whitespace-nowrap align-top">
                        <Mono className="text-kumo-strong">{shortId(tId)}</Mono>
                      </Table.Cell>
                      <Table.Cell className="align-top">
                        <span className="flex flex-wrap gap-1">
                          {t.from.map((s) => <Mono key={s} className="text-kumo-subtle">{shortId(s)}</Mono>)}
                        </span>
                      </Table.Cell>
                      <Table.Cell className="align-top"><Mono className="text-kumo-subtle">{shortId(t.to)}</Mono></Table.Cell>
                      <Table.Cell className="min-w-[280px] align-top">
                        {effects.length ? (
                          <ul className="space-y-2">
                            {effects.map(([eid, e]) => <SideEffectItem key={eid} effectId={eid} effect={e} />)}
                          </ul>
                        ) : (
                          <span className="text-xs text-kumo-inactive">none</span>
                        )}
                      </Table.Cell>
                      <Table.Cell className="align-top">
                        {refs.length ? (
                          <ul className="space-y-1">
                            {refs.map((r, i) => (
                              <li key={i} className="whitespace-nowrap text-xs">
                                <div><IdLink id={r.transaction}>{shortId(r.transaction)}</IdLink></div>
                                <div className="text-kumo-subtle">
                                  step {r.step + 1} in <IdLink id={r.operation}>{shortId(r.operation)}</IdLink>
                                </div>
                              </li>
                            ))}
                          </ul>
                        ) : (
                          <span className="text-xs text-kumo-inactive">no transaction</span>
                        )}
                      </Table.Cell>
                      <Table.Cell className="align-top">
                        {(obligations.get(`${id}/${tId}`) ?? []).length > 0 ? (
                          <StatusChips obKey={`${id}/${tId}`} />
                        ) : (
                          <span className="text-kumo-inactive">—</span>
                        )}
                      </Table.Cell>
                    </Table.Row>
                  );
                })}
              </Table.Body>
            </Table>
          </div>
        </SectionCard>
      </div>
    </div>
  );
}
