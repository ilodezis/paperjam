import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { I18nextProvider, useTranslation } from "react-i18next";
import { http, HttpResponse } from "msw";
import { setupServer } from "msw/node";
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { api, type AdminSettingsView } from "../api";
import { createI18n } from "../i18n";
import { mockUser } from "../mocks/data";
import { handlers } from "../mocks/handlers";
import { InboxSettingsSection } from "./InboxSettingsSection";
import { LlmSettingsSection } from "./LlmSettingsSection";
import { clearToasts, getToastsSnapshot } from "./controls/toast";

const server = setupServer(...handlers);
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root | null = null;
let container: HTMLDivElement | null = null;

beforeAll(() => server.listen());
afterEach(async () => {
  server.resetHandlers();
  clearToasts();
  vi.restoreAllMocks();
  if (root) await act(async () => root?.unmount());
  container?.remove();
  root = null;
  container = null;
  await api.updateSettings({ reset_llm: true, reset_calendar: true, reset_inbox: true });
});
afterAll(() => server.close());

function LlmHarness({
  confirm,
  onError,
}: {
  confirm: ReturnType<typeof vi.fn>;
  onError?: (code: string | null) => void;
}) {
  const { t } = useTranslation();
  const [error, setError] = useState<string | null>(null);
  return (
    <>
      {error ? <div role="alert">{t(`errors.${error}`, { defaultValue: t("errors.settings") })}</div> : null}
      <LlmSettingsSection
        confirm={confirm as never}
        onError={(code) => {
          setError(code);
          onError?.(code);
        }}
      />
    </>
  );
}

function InboxHarness({
  onError,
}: {
  onError?: (code: string | null) => void;
}) {
  const { t } = useTranslation();
  const [error, setError] = useState<string | null>(null);
  return (
    <>
      {error ? <div role="alert">{t(`errors.${error}`, { defaultValue: t("errors.settings") })}</div> : null}
      <InboxSettingsSection
        onError={(code) => {
          setError(code);
          onError?.(code);
        }}
      />
    </>
  );
}

async function renderNode(node: React.ReactNode, language = "en") {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  await act(async () => {
    root?.render(
      <QueryClientProvider client={queryClient}>
        <I18nextProvider i18n={createI18n(language)}>
          {node}
        </I18nextProvider>
      </QueryClientProvider>,
    );
  });
}

function byLabel<T extends HTMLElement = HTMLElement>(label: string): T | null {
  return (Array.from(container?.querySelectorAll<HTMLElement>("[aria-label]") ?? [])
    .find((element) => element.getAttribute("aria-label") === label) as T | undefined) ?? null;
}

function findButton(name: string): HTMLButtonElement | null {
  return Array.from(container?.querySelectorAll("button") ?? [])
    .find((element) => element.textContent?.trim() === name || element.getAttribute("aria-label") === name) ?? null;
}

async function click(element: Element) {
  await act(async () => { element.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
}

async function changeText(element: HTMLInputElement | HTMLTextAreaElement, value: string) {
  const prototype = element instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  Object.getOwnPropertyDescriptor(prototype, "value")?.set?.call(element, value);
  await act(async () => { element.dispatchEvent(new Event("input", { bubbles: true })); });
}

async function eventually(assertion: () => void | Promise<void>, timeout = 1500) {
  const started = Date.now();
  for (;;) {
    try {
      await act(async () => { await assertion(); });
      return;
    } catch (error) {
      if (Date.now() - started > timeout) throw error;
      await act(async () => { await new Promise((resolve) => setTimeout(resolve, 10)); });
    }
  }
}

describe("LlmSettingsSection", () => {
  it("renders initial environment-configured LLM settings and saves edits", async () => {
    await renderNode(<LlmHarness confirm={vi.fn().mockResolvedValue(true)} />);
    await eventually(() => expect(findButton("Save LLM settings")).not.toBeNull());

    expect(container?.textContent).toContain("Active");
    expect(container?.textContent).toContain("Configured via environment");
    expect(byLabel<HTMLInputElement>("API key")?.type).toBe("password");
    expect(byLabel<HTMLInputElement>("API key")?.value).toBe("");
    expect(findButton("Clear saved key")).toBeNull();

    const saveButton = findButton("Save LLM settings")!;
    expect(saveButton.disabled).toBe(true);

    await changeText(byLabel<HTMLInputElement>("Primary model")!, "openai/gpt-5-mini");
    await changeText(byLabel<HTMLInputElement>("Monthly budget (USD)")!, "12.50");
    await click(byLabel("Auto-create tags")!);
    expect(container?.textContent).toContain("Unsaved changes");
    expect(saveButton.disabled).toBe(false);

    await click(saveButton);
    await eventually(() => expect(saveButton.disabled).toBe(true));
    expect(getToastsSnapshot().map((item) => item.message)).toContain("LLM settings saved");

    const updated = await api.adminSettings();
    expect(updated.llm).toMatchObject({
      model: "openai/gpt-5-mini",
      monthly_budget_usd: "12.50",
      create_tags: false,
      has_overrides: true,
    });
    const summary = await api.settingsSummary();
    expect(summary).toMatchObject({
      llm_model: "openai/gpt-5-mini",
      llm_monthly_budget_usd: "12.50",
    });
  });

  it("handles write-only API key entry, shows Clear saved key, and clears DB key back to env", async () => {
    await renderNode(<LlmHarness confirm={vi.fn().mockResolvedValue(true)} />);
    await eventually(() => expect(byLabel("API key")).not.toBeNull());

    const keyInput = byLabel<HTMLInputElement>("API key")!;
    expect(keyInput.autocomplete).toBe("new-password");
    await changeText(keyInput, "sk-or-test-12345");
    await click(findButton("Save LLM settings")!);

    await eventually(() => expect(container?.textContent).toContain("Saved in database (encrypted)"));
    expect(byLabel<HTMLInputElement>("API key")?.value).toBe("");
    expect(findButton("Clear saved key")).not.toBeNull();

    await click(findButton("Clear saved key")!);
    await eventually(() => expect(container?.textContent).toContain("Configured via environment"));
    expect(findButton("Clear saved key")).toBeNull();
    expect(getToastsSnapshot().map((item) => item.message)).toContain("Saved API key cleared");
  });

  it("updates advanced provider, base URL, reasoning effort, allowed models, prices, and resets with confirmation", async () => {
    const confirm = vi.fn()
      .mockResolvedValueOnce(false)
      .mockResolvedValueOnce(true);
    await renderNode(<LlmHarness confirm={confirm} />);
    await eventually(() => expect(findButton("Save LLM settings")).not.toBeNull());

    const details = container?.querySelector<HTMLDetailsElement>(".settings-advanced-details");
    expect(details?.open).toBe(false);
    await click(details!.querySelector("summary")!);
    expect(details?.open).toBe(true);
    expect(findButton("Reset to env defaults")).toBeNull();

    await click(byLabel("Provider")!);
    await click(Array.from(container?.querySelectorAll<HTMLElement>('[role="option"]') ?? []).find((el) => el.textContent === "OpenAI-compatible")!);
    await changeText(byLabel<HTMLInputElement>("Base URL")!, "https://llm.example.invalid/v1");
    await changeText(byLabel<HTMLInputElement>("Vision model")!, "custom/vision");
    await click(byLabel("Reasoning effort")!);
    await click(Array.from(container?.querySelectorAll<HTMLElement>('[role="option"]') ?? []).find((el) => el.textContent === "Medium")!);
    await changeText(byLabel<HTMLTextAreaElement>("Allowed models")!, "model/a, model/b\nmodel/c");
    await changeText(byLabel<HTMLInputElement>("Model prices")!, "model/a=0.5:1.5");
    await click(findButton("Save LLM settings")!);

    await eventually(() => expect(findButton("Reset to env defaults")).not.toBeNull());
    const saved = await api.adminSettings();
    expect(saved.llm).toMatchObject({
      provider: "generic",
      effective_provider: "generic",
      base_url: "https://llm.example.invalid/v1",
      vision_model: "custom/vision",
      reasoning_effort: "medium",
      allowed_models: ["model/a", "model/b", "model/c"],
      prices: "model/a=0.5:1.5",
      has_overrides: true,
    });

    await click(findButton("Reset to env defaults")!);
    expect(confirm).toHaveBeenCalledTimes(1);
    expect(confirm.mock.calls[0]?.[0]).toMatchObject({ destructive: true, cancelLabel: "Cancel" });
    expect((await api.adminSettings()).llm.has_overrides).toBe(true);

    await click(findButton("Reset to env defaults")!);
    await eventually(() => expect(findButton("Reset to env defaults")).toBeNull());
    expect((await api.adminSettings()).llm.has_overrides).toBe(false);
    expect(getToastsSnapshot().map((item) => item.message)).toContain("LLM settings reset to defaults");
    expect(byLabel<HTMLInputElement>("Primary model")?.value).toBe("anthropic/claude-haiku-5.5");
  });

  it("shows Paused and Not configured badges, surfaces validation errors, and hides on 403", async () => {
    const onError = vi.fn();
    await renderNode(<LlmHarness confirm={vi.fn()} onError={onError} />);
    await eventually(() => expect(byLabel("Base URL")).not.toBeNull());

    await click(container!.querySelector(".settings-advanced-details summary")!);
    await changeText(byLabel<HTMLInputElement>("Base URL")!, "not-a-url");
    await click(findButton("Save LLM settings")!);
    await eventually(() => expect(onError).toHaveBeenCalledWith("invalid_llm_base_url"));
    expect(container?.querySelector('[role="alert"]')?.textContent).toContain("Base URL must be a valid http or https address.");

    server.use(
      http.get("/api/v1/settings", () => HttpResponse.json({
        llm: {
          enabled: false,
          has_api_key: false,
          api_key_source: "none",
          base_url: "https://openrouter.ai/api/v1",
          provider: "auto",
          effective_provider: "openrouter",
          model: "anthropic/claude-haiku-5.5",
          vision_model: "anthropic/claude-haiku-5.5",
          handwriting_enabled: false,
          handwriting_model: "google/gemini-2.5-flash",
          allowed_models: [],
          reasoning_effort: null,
          create_tags: true,
          monthly_budget_usd: null,
          prices: "",
          has_overrides: false,
        },
        calendar: { lead_days: 60, has_overrides: false },
        inbox: { configured: false, paused: false, user_id: null, interval_secs: 15, failed_files: [], has_overrides: false },
        ocr_langs: "eng",
      } satisfies AdminSettingsView)),
    );
    await act(async () => root?.unmount());
    container?.remove();
    await renderNode(<LlmHarness confirm={vi.fn()} />);
    await eventually(() => expect(container?.textContent).toContain("Not configured"));
    expect(container?.textContent).not.toContain("Disabled");

    server.use(
      http.get("/api/v1/settings", () => HttpResponse.json({ code: "admin_required", message: "Forbidden" }, { status: 403 })),
    );
    await act(async () => root?.unmount());
    container?.remove();
    await renderNode(<LlmHarness confirm={vi.fn()} />);
    await eventually(() => expect(container?.querySelector("section")).toBeNull());
  });
});

describe("InboxSettingsSection", () => {
  it("shows informational card when watch folder is not configured", async () => {
    await renderNode(<InboxHarness />);
    await eventually(() => expect(container?.textContent).toContain("Watch folder is not mounted"));
    expect(container?.textContent).toContain("PAPERJAM_INBOX_DIR");
    expect(findButton("Save inbox settings")).toBeNull();
  });

  it("edits pause, user assignment, poll interval, validates interval, and renders failed files", async () => {
    let inboxState: AdminSettingsView["inbox"] = {
      configured: true,
      paused: false,
      user_id: null,
      interval_secs: 15,
      failed_files: [
        {
          name: "broken-scan.bin",
          reason: "Unsupported file format",
          size: 2048,
          failed_at: "2026-02-14T10:30:00Z",
        },
      ],
      has_overrides: false,
    };

    server.use(
      http.get("/api/v1/settings", () => HttpResponse.json({
        llm: {
          enabled: true,
          has_api_key: true,
          api_key_source: "env",
          base_url: "https://openrouter.ai/api/v1",
          provider: "auto",
          effective_provider: "openrouter",
          model: "anthropic/claude-haiku-5.5",
          vision_model: "anthropic/claude-haiku-5.5",
          handwriting_enabled: true,
          handwriting_model: "google/gemini-2.5-flash",
          allowed_models: [],
          reasoning_effort: null,
          create_tags: true,
          monthly_budget_usd: null,
          prices: "",
          has_overrides: false,
        },
        calendar: { lead_days: 60, has_overrides: false },
        inbox: inboxState,
        ocr_langs: "eng",
      } satisfies AdminSettingsView)),
      http.patch("/api/v1/settings", async ({ request }) => {
        const patch = await request.json() as { inbox?: { paused?: boolean; user_id?: string | null; interval_secs?: number } };
        if (patch.inbox?.interval_secs !== undefined && (patch.inbox.interval_secs < 1 || patch.inbox.interval_secs > 86400)) {
          return HttpResponse.json({ code: "invalid_inbox_interval", message: "Bad interval" }, { status: 422 });
        }
        inboxState = {
          ...inboxState,
          paused: patch.inbox?.paused ?? inboxState.paused,
          user_id: "user_id" in (patch.inbox ?? {}) ? (patch.inbox?.user_id ?? null) : inboxState.user_id,
          interval_secs: patch.inbox?.interval_secs ?? inboxState.interval_secs,
          has_overrides: true,
        };
        return HttpResponse.json({
          llm: {
            enabled: true,
            has_api_key: true,
            api_key_source: "env",
            base_url: "https://openrouter.ai/api/v1",
            provider: "auto",
            effective_provider: "openrouter",
            model: "anthropic/claude-haiku-5.5",
            vision_model: "anthropic/claude-haiku-5.5",
            handwriting_enabled: true,
            handwriting_model: "google/gemini-2.5-flash",
            allowed_models: [],
            reasoning_effort: null,
            create_tags: true,
            monthly_budget_usd: null,
            prices: "",
            has_overrides: false,
          },
          calendar: { lead_days: 60, has_overrides: false },
          inbox: inboxState,
          ocr_langs: "eng",
        } satisfies AdminSettingsView);
      }),
    );

    const onError = vi.fn();
    await renderNode(<InboxHarness onError={onError} />);
    await eventually(() => expect(findButton("Save inbox settings")).not.toBeNull());

    expect(container?.textContent).toContain("Watching");
    expect(container?.textContent).toContain("broken-scan.bin");
    expect(container?.textContent).toContain("Unsupported file format");
    expect(container?.textContent).toContain("2.0");

    await click(byLabel("Pause inbox scanning")!);
    await click(byLabel("Assign documents to")!);
    await eventually(() => expect(Array.from(container?.querySelectorAll('[role="option"]') ?? []).some((el) => el.textContent === mockUser.display_name)).toBe(true));
    await click(Array.from(container?.querySelectorAll<HTMLElement>('[role="option"]') ?? []).find((el) => el.textContent === mockUser.display_name)!);
    await changeText(byLabel<HTMLInputElement>("Poll interval (seconds)")!, "30");
    await click(findButton("Save inbox settings")!);

    await eventually(() => expect(container?.textContent).toContain("Paused"));
    expect(inboxState).toMatchObject({ paused: true, user_id: mockUser.id, interval_secs: 30 });
    expect(getToastsSnapshot().map((item) => item.message)).toContain("Inbox settings saved");

    await changeText(byLabel<HTMLInputElement>("Poll interval (seconds)")!, "0");
    await click(findButton("Save inbox settings")!);
    await eventually(() => expect(onError).toHaveBeenCalledWith("invalid_inbox_interval"));
    expect(container?.querySelector('[role="alert"]')?.textContent).toContain("Poll interval must be an integer between 1 and 86400 seconds.");

    inboxState = { ...inboxState, failed_files: [] };
    await act(async () => root?.unmount());
    container?.remove();
    await renderNode(<InboxHarness />);
    await eventually(() => expect(container?.textContent).toContain("No rejected files."));
  });

  it("hides itself on 403", async () => {
    server.use(
      http.get("/api/v1/settings", () => HttpResponse.json({ code: "admin_required", message: "Forbidden" }, { status: 403 })),
    );
    await renderNode(<InboxHarness />);
    await eventually(() => expect(container?.querySelector("section")).toBeNull());
  });
});
