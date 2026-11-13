import { createContext, use } from "react";

/** The shape the layout is in. The app is one container; from 641px it is a rail beside a stage,
 *  below that a stage over a bottom bar. CSS answers this with media queries; the few things that
 *  are drawn in script (which gestures run, whether the previous screen stays mounted) ask here. */
export interface Layout {
  desktop: boolean;
}

export const LayoutContext = createContext<Layout>({ desktop: false });

export const useLayout = (): Layout => use(LayoutContext);

/** The container width from which the rail replaces the bottom bar. Mirrors styles.css. */
export const DESKTOP_MIN_WIDTH = 641;
