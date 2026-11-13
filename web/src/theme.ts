import { useSyncExternalStore } from "react";

export type ThemePreference = "system" | "light" | "dark";
export type ResolvedTheme = Exclude<ThemePreference, "system">;

type MediaQuery = (query: string) => Pick<MediaQueryList, "matches">;

const STORAGE_KEY = "paperjam:theme";

export function resolveTheme(preference: ThemePreference, media: MediaQuery = window.matchMedia): ResolvedTheme {
  if (preference !== "system") return preference;
  return media("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

export function readThemePreference(): ThemePreference {
  const value = window.localStorage.getItem(STORAGE_KEY);
  return value === "light" || value === "dark" || value === "system" ? value : "system";
}

export function applyThemePreference(preference: ThemePreference): void {
  window.localStorage.setItem(STORAGE_KEY, preference);
  document.documentElement.dataset.theme = resolveTheme(preference);
  document.documentElement.style.colorScheme = resolveTheme(preference);
}

const listeners = new Set<() => void>();

/** Apply a preference and tell everything that shows it. */
export function setThemePreference(preference: ThemePreference): void {
  applyThemePreference(preference);
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useThemePreference(): ThemePreference {
  return useSyncExternalStore(subscribe, readThemePreference, () => "system" as const);
}
