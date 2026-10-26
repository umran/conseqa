import type { TransactionProofs } from "./transactionProofs";
import type { Graph } from "./graph";
import type { Model } from "./model";
import type { ProverReport } from "./report";

// The page data the Rust renderer injects as `window.CONSEQA`, or serves
// as `conseqa.json` during development.
export interface PageData {
  title: string;
  model: Model;
  graph: Graph;
  /** The transaction proofs — the serializability and ordering
   *  arguments — drawn from the model. */
  transaction_proofs: TransactionProofs;
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
        "Provide public/conseqa.json, or open the spec.html produced by " +
        "the design MCP server's export_spec tool.",
    );
  }

  return (await response.json()) as PageData;
}
