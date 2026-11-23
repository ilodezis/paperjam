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

  const savedLanguage = window.localStorage.getItem("paperjam:language");
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
