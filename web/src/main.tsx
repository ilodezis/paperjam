import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { I18nextProvider } from "react-i18next";

import { App } from "./app/App";
import { createI18n, pickLanguage } from "./i18n";
import { registerServiceWorker } from "./pwa";
import { applyThemePreference, readThemePreference } from "./theme";
import "./fonts";
import "./styles.css";

async function start() {
  if (import.meta.env.DEV && import.meta.env.VITE_API_MODE !== "live") {
    const { worker } = await import("./mocks/browser");
    await worker.start({ serviceWorker: { url: "/mockServiceWorker.js" } });
  } else {
    void registerServiceWorker();
  }

  // The faces the interface draws with, ready before the first paint: a late face swaps in for the
  // fallback and moves the text by a pixel or two. Capped, so a slow font never holds the page.
  if ("fonts" in document) {
    const sample = "Aa Яя";
    const faces = ['400 1em "Golos Text"', '500 1em "Golos Text"', '600 1em "Golos Text"', '700 1em "Golos Text"', '500 1em "Geologica"', '600 1em "Geologica"'];
    await Promise.race([
      Promise.all(faces.map((face) => document.fonts.load(face, sample).catch(() => []))),
      new Promise((resolve) => window.setTimeout(resolve, 1500)),
    ]);
  }

  const savedLanguage =window.localStorage.getItem("paperjam:language");
  const i18n = createI18n(pickLanguage([savedLanguage ?? "", ...navigator.languages]));
  document.documentElement.lang = i18n.language;
  applyThemePreference(readThemePreference());

  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <I18nextProvider i18n={i18n}>
        <App />
      </I18nextProvider>
    </StrictMode>,
  );
}

void start();
