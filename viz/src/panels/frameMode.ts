import { createContext, useContext } from "react";

/** Where a detail is being drawn: in the inspector beside the canvas,
 *  or as a page in the canvas itself. The same components serve both;
 *  the frame around them differs. */
export type FrameMode = "panel" | "page";

export const FrameModeContext = createContext<FrameMode>("panel");

export function useFrameMode(): FrameMode {
  return useContext(FrameModeContext);
}
