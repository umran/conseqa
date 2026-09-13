import { useCallback, useEffect, useRef, useState } from "react";
import type { PointerEvent } from "react";

export interface ViewBox {
  x: number;
  y: number;
  w: number;
  h: number;
}

const MIN_W = 120;
const MAX_W = 50000;

/** Pan/zoom state for an SVG canvas, expressed as its viewBox. */
export function useViewBox(svgRef: React.RefObject<SVGSVGElement | null>) {
  const [viewBox, setViewBox] = useState<ViewBox>({ x: 0, y: 0, w: 1000, h: 700 });
  const drag = useRef<{
    sx: number;
    sy: number;
    vb: ViewBox;
    moved: boolean;
    pointerId: number;
  } | null>(null);
  const suppressClick = useRef(false);

  const clientToWorld = useCallback(
    (cx: number, cy: number, vb: ViewBox) => {
      const rect = svgRef.current?.getBoundingClientRect();
      if (!rect) return { x: vb.x, y: vb.y };
      return {
        x: vb.x + ((cx - rect.left) / rect.width) * vb.w,
        y: vb.y + ((cy - rect.top) / rect.height) * vb.h,
      };
    },
    [svgRef],
  );

  // Registered natively, and explicitly not passively.
  //
  // React attaches `wheel` at the root as a *passive* listener, where
  // `preventDefault` does nothing at all: the graph would zoom, and the
  // browser would zoom the whole page underneath it at the same time.
  // A trackpad pinch arrives here too — as a wheel event carrying
  // `ctrlKey` — and that is the gesture the page would otherwise steal.
  useEffect(() => {
    const svg = svgRef.current;
    if (!svg) return;

    const onWheel = (event: globalThis.WheelEvent) => {
      event.preventDefault();

      setViewBox((vb) => {
        // A pinch reports far smaller deltas than a wheel notch for the
        // same intent, so it needs a longer lever to feel like one
        // gesture rather than a nudge.
        const sensitivity = event.ctrlKey ? 0.01 : 0.0015;
        const next = Math.min(MAX_W, Math.max(MIN_W, vb.w * Math.exp(event.deltaY * sensitivity)));
        const real = next / vb.w;
        const p = clientToWorld(event.clientX, event.clientY, vb);

        return {
          x: p.x - (p.x - vb.x) * real,
          y: p.y - (p.y - vb.y) * real,
          w: vb.w * real,
          h: vb.h * real,
        };
      });
    };

    svg.addEventListener("wheel", onWheel, { passive: false });
    return () => svg.removeEventListener("wheel", onWheel);
  }, [svgRef, clientToWorld]);

  // The canvas changes size whenever a panel opens or closes beside it —
  // the inspector when something is selected, the obligations panel, the
  // navigator. Left alone, the browser would fit the same viewBox into
  // the new box, and the drawing would shift and rescale under the
  // reader's eyes at the moment they clicked. Instead the world-to-screen
  // mapping is held fixed: the scale stays what it was, and the world
  // point under any screen point stays under it, so a panel covers or
  // uncovers drawing and never moves it. A deliberate re-fit — the fit
  // button, a layer switch, a panel toggle — is the caller's to request.
  useEffect(() => {
    const svg = svgRef.current;
    if (!svg || typeof ResizeObserver === "undefined") return;
    let last = svg.getBoundingClientRect();
    const observer = new ResizeObserver(() => {
      const rect = svg.getBoundingClientRect();
      const prev = last;
      last = rect;
      if (prev.width < 1 || prev.height < 1 || rect.width < 1 || rect.height < 1) return;
      if (rect.width === prev.width && rect.height === prev.height && rect.left === prev.left && rect.top === prev.top) return;
      setViewBox((vb) => {
        const scale = vb.w / prev.width;
        return {
          x: vb.x + (rect.left - prev.left) * scale,
          y: vb.y + (rect.top - prev.top) * scale,
          w: rect.width * scale,
          h: rect.height * scale,
        };
      });
    });
    observer.observe(svg);
    return () => observer.disconnect();
  }, [svgRef]);

  const onPointerDown = useCallback(
    (e: PointerEvent<SVGSVGElement>) => {
      if (e.button !== 0) return;
      drag.current = { sx: e.clientX, sy: e.clientY, vb: viewBox, moved: false, pointerId: e.pointerId };
    },
    [viewBox],
  );

  const onPointerMove = useCallback(
    (e: PointerEvent<SVGSVGElement>) => {
      const d = drag.current;
      if (!d) return;
      if (!d.moved) {
        if (Math.abs(e.clientX - d.sx) + Math.abs(e.clientY - d.sy) <= 5) return;
        d.moved = true;
        svgRef.current?.setPointerCapture(d.pointerId);
      }
      const rect = svgRef.current?.getBoundingClientRect();
      if (!rect) return;
      const dx = ((e.clientX - d.sx) / rect.width) * d.vb.w;
      const dy = ((e.clientY - d.sy) / rect.height) * d.vb.h;
      setViewBox({ ...d.vb, x: d.vb.x - dx, y: d.vb.y - dy });
    },
    [svgRef],
  );

  const onPointerUp = useCallback(() => {
    if (drag.current?.moved) {
      suppressClick.current = true;
      setTimeout(() => {
        suppressClick.current = false;
      }, 0);
    }
    drag.current = null;
  }, []);

  /** Fits the given scene bounds into the canvas with padding. */
  const fitBounds = useCallback(
    (b: { x: number; y: number; width: number; height: number }, pad = 60) => {
      if (b.width === 0 && b.height === 0) return;
      const rect = svgRef.current?.getBoundingClientRect();
      const aspect = rect && rect.width > 1 && rect.height > 1 ? rect.width / rect.height : 16 / 9;
      let w = b.width + pad * 2;
      let h = b.height + pad * 2;
      if (w / h < aspect) w = h * aspect;
      else h = w / aspect;
      setViewBox({ x: b.x + b.width / 2 - w / 2, y: b.y + b.height / 2 - h / 2, w, h });
    },
    [svgRef],
  );

  return {
    viewBox,
    isDragging: () => drag.current?.moved ?? false,
    wasDragged: () => suppressClick.current,
    handlers: { onPointerDown, onPointerMove, onPointerUp, onPointerCancel: onPointerUp },
    fitBounds,
  };
}
