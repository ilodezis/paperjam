import { readFileSync } from "node:fs";
import { createRequire } from "node:module";

import react from "@vitejs/plugin-react";
import type { Plugin } from "vite";
import { defineConfig } from "vitest/config";

const require = createRequire(import.meta.url);

/** The few faces the server's own pages (the OAuth consent screen) load from `/fonts/`. They live
 *  outside the app bundle under fixed names, because a page rendered by the server cannot know
 *  the hashed names Vite gives the app's copies. */
const SERVER_FONTS = [
  "@fontsource/golos-text/files/golos-text-latin-400-normal.woff2",
  "@fontsource/golos-text/files/golos-text-cyrillic-400-normal.woff2",
  "@fontsource/golos-text/files/golos-text-latin-600-normal.woff2",
  "@fontsource/golos-text/files/golos-text-cyrillic-600-normal.woff2",
  "@fontsource/geologica/files/geologica-latin-600-normal.woff2",
  "@fontsource/geologica/files/geologica-cyrillic-600-normal.woff2",
];

function serverFonts(): Plugin {
  return {
    name: "paperjam-server-fonts",
    apply: "build",
    generateBundle() {
      for (const font of SERVER_FONTS) {
        this.emitFile({
          type: "asset",
          fileName: `fonts/${font.split("/").pop()}`,
          source: readFileSync(require.resolve(font)),
        });
      }
    },
  };
}

/** Fontsource lists a `.woff` fallback after every `.woff2`; no browser the app supports asks for
 *  it, and Vite would copy every one into the bundle anyway. Dropped from the stylesheets before
 *  Vite sees the `url()`s. */
function woff2Only(): Plugin {
  return {
    name: "paperjam-woff2-only",
    enforce: "pre",
    transform(code, id) {
      if (!/[\\/]@fontsource[\\/].*\.css$/.test(id)) return null;
      return code.replace(/,\s*url\([^)]*\.woff\)\s*format\(['"]woff['"]\)/g, "");
    },
  };
}

export default defineConfig({
  plugins: [react(), woff2Only(), serverFonts()],
  build: { outDir: "dist", emptyOutDir: true },
  server: { proxy: { "/api": "http://127.0.0.1:8140" } },
  test: { environment: "jsdom" },
});
