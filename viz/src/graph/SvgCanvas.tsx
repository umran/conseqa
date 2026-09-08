import { Button } from "@cloudflare/kumo/components/button";
import { Tooltip } from "@cloudflare/kumo/components/tooltip";
import { ArrowsOutIcon } from "@phosphor-icons/react";
import { Empty } from "@cloudflare/kumo/components/empty";
import { GraphIcon } from "@phosphor-icons/react";
import { useEffect, useLayoutEffect, useRef, type MouseEvent, type ReactNode } from "react";

import { routeKey } from "../lib/route";
import { useApp, type DetailContext } from "../state/AppState";
import { useViewBox } from "./useViewBox";

export interface SelectionPayload {
  key: string;
  id: string;
  ctx?: DetailContext;
}

/** Serialized into `data-sel` so the canvas can delegate clicks. */
export function sel(payload: SelectionPayload): string {
  return JSON.stringify(payload);
}

interface Props {
  children: ReactNode;
  legend: ReactNode;
  empty?: string | null;
}

/**
 * The pan/zoom SVG stage. Click handling is delegated through data
 * attributes so the views stay declarative:
 *   data-sel  — select + show detail (JSON SelectionPayload)
 *   data-nav  — navigate to a hash
 *   data-act  — toggle a transaction expansion (JSON {key})
 *   data-dbl  — navigate on double-click
 */
export function SvgCanvas({ children, legend, empty }: Props) {
  const app = useApp();
  const svgRef = useRef<SVGSVGElement>(null);
  const sceneRef = useRef<SVGGElement>(null);
  const { viewBox, wasDragged, handlers, fitBounds } = useViewBox(svgRef);
  const fitted = useRef<Set<string>>(new Set());
  const key = routeKey(app.route);

  const fitScene = () => {
    const scene = sceneRef.current;
    if (!scene) return;
    try {
      fitBounds(scene.getBBox());
    } catch {
      // Detached or zero-size scenes have no bounding box.
    }
  };

  // Fit once per route, after layout dimensions exist. The route is
  // marked only when the fit actually runs, so a cancelled frame (for
  // instance StrictMode's development re-run) does not consume it.
  useLayoutEffect(() => {
    if (fitted.current.has(key)) return;
    const frame = requestAnimationFrame(() => {
      fitted.current.add(key);
      fitScene();
    });
    return () => cancelAnimationFrame(frame);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);

  useEffect(() => {
    if (app.fitRequest > 0) fitScene();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [app.fitRequest]);

  const onClick = (e: MouseEvent<SVGSVGElement>) => {
    if (wasDragged()) return;
    const target = e.target as Element;
    const nav = target.closest("[data-nav]");
    if (nav) {
      app.navigateTo(nav.getAttribute("data-nav")!);
      return;
    }
    const act = target.closest("[data-act]");
    if (act) {
      const { key: txKey } = JSON.parse(act.getAttribute("data-act")!) as { key: string };
      app.toggleTx(txKey);
      return;
    }
    const selected = target.closest("[data-sel]");
    if (selected) {
      const payload = JSON.parse(selected.getAttribute("data-sel")!) as SelectionPayload;
      app.select(payload.key, { id: payload.id, ctx: payload.ctx ?? {} });
      return;
    }
    if (app.selection) app.select(null);
  };

  const onDoubleClick = (e: MouseEvent<SVGSVGElement>) => {
    const dbl = (e.target as Element).closest("[data-dbl]");
    if (dbl) app.navigateTo(dbl.getAttribute("data-dbl")!);
  };

  return (
    <div className="relative h-full w-full">
      <svg
        ref={svgRef}
        className="arch-canvas"
        xmlns="http://www.w3.org/2000/svg"
        viewBox={`${viewBox.x} ${viewBox.y} ${viewBox.w} ${viewBox.h}`}
        onClick={onClick}
        onDoubleClick={onDoubleClick}
        {...handlers}
      >
        <Defs />
        <g id="scene" ref={sceneRef}>
          {children}
        </g>
      </svg>
      {/* The fit control belongs to the canvas it acts on. */}
      <div className="absolute bottom-4 right-4">
        <Tooltip
          content="Fit graph to view"
          render={<Button variant="secondary" size="sm" shape="square" icon={ArrowsOutIcon} aria-label="Fit graph to view" onClick={app.requestFit} />}
        />
      </div>
      {/* Top left, not bottom left: the foot of the canvas is where the
          L1 band and its label sit, and a legend that covers the drawing
          it explains is worse than no legend. */}
      <div className="pointer-events-none absolute left-4 top-4 flex flex-col gap-1 rounded-lg border border-kumo-hairline bg-kumo-base/90 px-3 py-2 text-xs text-kumo-subtle backdrop-blur">
        {legend}
      </div>
      {empty ? (
        <div className="absolute inset-0 flex items-center justify-center">
          <Empty size="sm" icon={<GraphIcon size={32} className="text-kumo-inactive" />} title={empty} />
        </div>
      ) : null}
    </div>
  );
}

function Defs() {
  const marker = (id: string, color: string) => (
    <marker
      key={id}
      id={id}
      markerWidth={9}
      markerHeight={7}
      refX={8}
      refY={3.5}
      orient="auto"
      markerUnits="userSpaceOnUse"
    >
      <path d="M0,0 L9,3.5 L0,7 Z" fill={color} />
    </marker>
  );
  return (
    <defs>
      {marker("arr-publish", "var(--arch-edge-publish)")}
      {marker("arr-subscribe", "var(--arch-edge-subscribe)")}
      {marker("arr-request", "var(--arch-edge-request)")}
      {marker("arr-external", "var(--arch-edge-external)")}
      {marker("arr-client", "var(--arch-edge-client)")}
      {marker("arr-neutral", "var(--arch-text-subtle)")}
      {marker("arr-accent", "var(--arch-accent)")}
      {marker("arr-faint", "var(--arch-text-faint)")}
      {marker("arr-l1", "var(--arch-l1)")}
    </defs>
  );
}

export function LegendLine({ color, label, dashed }: { color: string; label: string; dashed?: boolean }) {
  return (
    <div className="flex items-center gap-2">
      <span
        className="inline-block h-0 w-5 border-t-2"
        style={{ borderColor: color, borderStyle: dashed ? "dashed" : "solid" }}
      />
      <span>{label}</span>
    </div>
  );
}

export function LegendChip({ color, label }: { color: string; label: string }) {
  return (
    <div className="flex items-center gap-2">
      <span
        className="inline-block h-3 w-3 rounded-sm border"
        style={{ borderColor: color, background: `color-mix(in srgb, ${color} 30%, transparent)` }}
      />
      <span>{label}</span>
    </div>
  );
}
