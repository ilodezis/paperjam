import i18next from "i18next";
import { initReactI18next } from "react-i18next";

import en from "./locales/en.json";
import ru from "./locales/ru.json";

export const resources = {
  en: { translation: en },
  ru: { translation: ru },
} as const;

/** The browser language if it is supported, English otherwise. */
export function pickLanguage(preferred: readonly string[]): "en" | "ru" {
  for (const tag of preferred) {
    const base = tag.toLowerCase().split("-")[0];
    if (base === "ru" || base === "en") return base;
  }
  return "en";
}

export function createI18n(language: string) {
  const instance = i18next.createInstance();
  void instance.use(initReactI18next).init({
    resources,
    lng: language,
    fallbackLng: "en",
    interpolation: { escapeValue: false },
    initAsync: false,
  });
  return instance;
}
