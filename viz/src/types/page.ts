import type { ConsistencyView } from "./consistency";
import type { Graph } from "./graph";
import type { Model } from "./model";
import type { ProverReport } from "./report";

// The page data `conseqa-viz` injects as `window.CONSEQA`, or serves
// as `conseqa.json` during development.
export interface PageData {
  title: string;
  model: Model;
  graph: Graph;
  /** The transaction consistency arguments, drawn from the model. */
  consistency: ConsistencyView;
  report: ProverReport | null;
}

declare global {
  interface Window {
    CONSEQA?: PageData;
  }
}

export async function loadPageData(): Promise<PageData> {
  if (window.CONSEQA) return window.CONSEQA;

  const response = await fetch("conseqa.json");

  if (!response.ok) {
    throw new Error(
      `no page data: ${response.status} ${response.statusText}. ` +
        "Run `npm run data` to generate public/conseqa.json, or open a " +
        "file produced by conseqa-viz.",
    );
  }

  return (await response.json()) as PageData;
}
