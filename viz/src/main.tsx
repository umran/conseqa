import { Empty } from "@cloudflare/kumo/components/empty";
import { WarningCircleIcon } from "@phosphor-icons/react";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "./App";
import "./index.css";
import { loadPageData } from "./types/page";

const root = createRoot(document.getElementById("root")!);

loadPageData().then(
  (data) => {
    root.render(
      <StrictMode>
        <App data={data} />
      </StrictMode>,
    );
  },
  (error: unknown) => {
    root.render(
      <div className="flex h-full items-center justify-center bg-kumo-canvas">
        <Empty
          icon={<WarningCircleIcon size={48} className="text-kumo-danger" />}
          title="No model data"
          description={error instanceof Error ? error.message : String(error)}
          commandLine="npm run data"
        />
      </div>,
    );
  },
);
