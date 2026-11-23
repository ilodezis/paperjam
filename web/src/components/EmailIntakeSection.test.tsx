import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { I18nextProvider, useTranslation } from "react-i18next";
import { http, HttpResponse } from "msw";
import { setupServer } from "msw/node";
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { api } from "../api";
import { createI18n } from "../i18n";
import { mockUser } from "../mocks/data";
import { handlers } from "../mocks/handlers";
import { EmailIntakeSection } from "./EmailIntakeSection";
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
  await api.revokeEmailSecret();
  await api.updateEmailSettings({
    user_id: null,
    allowed_senders: [],
    body_fallback: true,
    local_only: false,
    tag: null,
  });
});
afterAll(() => server.close());

function Harness({
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
      <EmailIntakeSection
        confirm={confirm as never}
        onError={(code) => {
          setError(code);
          onError?.(code);
        }}
      />
    </>
  );
}

async function renderSection(
  confirm: ReturnType<typeof vi.fn> = vi.fn().mockResolvedValue(true),
  onError?: (code: string | null) => void,
) {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  await act(async () => {
    root?.render(
      <QueryClientProvider client={queryClient}>
        <I18nextProvider i18n={createI18n("en")}>
          <Harness confirm={confirm} onError={onError} />
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

describe("EmailIntakeSection", () => {
  it("renders initial unconfigured state with disabled enable switch and empty-senders note", async () => {
    await renderSection();
    await eventually(() => expect(findButton("Generate secret")).not.toBeNull());

    expect(container?.textContent).toContain("Inactive");
    const enableSwitch = byLabel<HTMLInputElement>("Accept incoming emails")!;
    expect(enableSwitch.disabled).toBe(true);
    expect(enableSwitch.checked).toBe(false);
    expect(container?.textContent).toContain("Generate a secret first before enabling email intake.");
    expect(byLabel<HTMLInputElement>("Email webhook endpoint URL")?.value).toMatch(/\/api\/v1\/ingest\/email$/);
    expect(container?.textContent).toContain("Empty list: any sender reaching this endpoint is accepted");

    const details = container?.querySelector<HTMLDetailsElement>(".email-worker-details");
    expect(details?.open).toBe(false);
    await click(details!.querySelector("summary")!);
    expect(details?.textContent).toContain("PAPERJAM_URL");
    expect(details?.textContent).toContain("EMAIL_SECRET");
    expect(details?.textContent).toContain("ALLOWED_SENDERS");
    expect(details?.textContent).toContain("POST /api/v1/ingest/email");
  });

  it("generates a secret without confirmation, enables intake, shows prefix, and copies values", async () => {
    const confirm = vi.fn().mockResolvedValue(true);
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });

    await renderSection(confirm);
    await eventually(() => expect(findButton("Generate secret")).not.toBeNull());

    await click(findButton("Generate secret")!);
    await eventually(() => expect(byLabel<HTMLInputElement>("Email intake secret")?.value).toMatch(/^pje_/));

    expect(confirm).not.toHaveBeenCalled();
    const secret = byLabel<HTMLInputElement>("Email intake secret")!.value;
    expect(container?.textContent).toContain(`${secret.slice(0, 12)}…`);
    expect(container?.textContent).toContain("0 emails received");
    expect(container?.textContent).toContain("Last received: Never");
    expect(getToastsSnapshot().map((item) => item.message)).toContain("Email secret created");

    const enableSwitch = byLabel<HTMLInputElement>("Accept incoming emails")!;
    expect(enableSwitch.disabled).toBe(false);
    expect(enableSwitch.checked).toBe(true);
    expect(container?.textContent).toContain("Active");
    expect((await api.emailSettings()).enabled).toBe(true);

    await click(enableSwitch);
    await eventually(() => expect(enableSwitch.checked).toBe(false));
    expect(container?.textContent).toContain("Inactive");
    expect((await api.emailSettings()).enabled).toBe(false);

    await click(byLabel("Copy secret")!);
    expect(writeText).toHaveBeenCalledWith(secret);
    await click(byLabel("Copy endpoint")!);
    expect(writeText).toHaveBeenCalledWith(byLabel<HTMLInputElement>("Email webhook endpoint URL")!.value);
  });

  it("rotates secret after confirmation, replaces secret, and preserves other settings", async () => {
    const firstCreated = await api.rotateEmailSecret();
    await api.updateEmailSettings({
      enabled: true,
      allowed_senders: ["billing@example.com"],
      tag: "receipts",
      body_fallback: true,
      local_only: true,
    });

    const confirm = vi.fn()
      .mockResolvedValueOnce(false)
      .mockResolvedValueOnce(true);

    await renderSection(confirm);
    await eventually(() => expect(byLabel("Rotate secret")).not.toBeNull());

    await click(byLabel("Rotate secret")!);
    expect(confirm).toHaveBeenCalledTimes(1);
    expect(confirm.mock.calls[0]?.[0]).toMatchObject({
      cancelLabel: "Cancel",
      destructive: true,
    });
    expect(byLabel("Email intake secret")).toBeNull();

    await click(byLabel("Rotate secret")!);
    await eventually(() => expect(byLabel<HTMLInputElement>("Email intake secret")?.value).toMatch(/^pje_/));

    const rotatedSecret = byLabel<HTMLInputElement>("Email intake secret")!.value;
    expect(rotatedSecret).not.toBe(firstCreated.secret);
    expect(container?.textContent).toContain(`${rotatedSecret.slice(0, 12)}…`);

    const saved = await api.emailSettings();
    expect(saved).toMatchObject({
      enabled: true,
      has_secret: true,
      secret_prefix: rotatedSecret.slice(0, 12),
      allowed_senders: ["billing@example.com"],
      tag: "receipts",
      body_fallback: true,
      local_only: true,
    });
  });

  it("revokes secret after destructive confirmation, clears secret and disables intake", async () => {
    await api.rotateEmailSecret();
    await api.updateEmailSettings({ enabled: true });

    const confirm = vi.fn().mockResolvedValue(true);
    await renderSection(confirm);
    await eventually(() => expect(byLabel("Revoke email secret")).not.toBeNull());

    await click(byLabel("Rotate secret")!);
    await eventually(() => expect(byLabel("Email intake secret")).not.toBeNull());

    await click(byLabel("Revoke email secret")!);
    await eventually(() => expect(findButton("Generate secret")).not.toBeNull());

    expect(confirm).toHaveBeenCalledTimes(2);
    expect(confirm.mock.calls[1]?.[0]).toMatchObject({
      cancelLabel: "Cancel",
      destructive: true,
    });
    expect(byLabel("Email intake secret")).toBeNull();
    expect(container?.textContent).toContain("Inactive");
    const enableSwitch = byLabel<HTMLInputElement>("Accept incoming emails")!;
    expect(enableSwitch.checked).toBe(false);
    expect(enableSwitch.disabled).toBe(true);

    const saved = await api.emailSettings();
    expect(saved).toMatchObject({
      enabled: false,
      has_secret: false,
      secret_prefix: null,
    });
  });

  it("edits allowed senders, target user, tag, body fallback and local-only switches and saves", async () => {
    await api.rotateEmailSecret();
    await renderSection();
    await eventually(() => expect(findButton("Save settings")).not.toBeNull());

    const saveButton = findButton("Save settings")!;
    expect(saveButton.disabled).toBe(true);

    await changeText(
      byLabel<HTMLTextAreaElement>("Allowed senders")!,
      "billing@example.com, @utility.example\nreceipts@example.org",
    );
    expect(container?.textContent).toContain("Unsaved changes");
    expect(container?.textContent).toContain("One email address or @domain per line");
    expect(saveButton.disabled).toBe(false);

    await click(byLabel("Assign documents to")!);
    await eventually(() => expect(Array.from(container?.querySelectorAll('[role="option"]') ?? []).some((el) => el.textContent === mockUser.display_name)).toBe(true));
    await click(Array.from(container?.querySelectorAll<HTMLElement>('[role="option"]') ?? []).find((el) => el.textContent === mockUser.display_name)!);

    await changeText(byLabel<HTMLInputElement>("Auto-tag")!, "email-inbox");
    await click(byLabel("Save email body when there are no attachments")!);
    await click(byLabel("Process emailed documents locally only")!);

    // Toggling the immediate enable switch keeps unsaved form edits intact.
    await click(byLabel("Accept incoming emails")!);
    await eventually(() => expect(byLabel<HTMLInputElement>("Accept incoming emails")?.checked).toBe(false));
    expect(byLabel<HTMLInputElement>("Auto-tag")?.value).toBe("email-inbox");
    expect(saveButton.disabled).toBe(false);

    await click(saveButton);
    await eventually(() => expect(saveButton.disabled).toBe(true));
    expect(container?.textContent).not.toContain("Unsaved changes");
    expect(getToastsSnapshot().map((item) => item.message)).toContain("Email intake settings saved");

    const updated = await api.emailSettings();
    expect(updated).toMatchObject({
      enabled: false,
      allowed_senders: ["billing@example.com", "@utility.example", "receipts@example.org"],
      user_id: mockUser.id,
      tag: "email-inbox",
      body_fallback: false,
      local_only: true,
    });
  });

  it("reports invalid_sender_rule error and keeps edits dirty", async () => {
    const onError = vi.fn();
    await renderSection(vi.fn().mockResolvedValue(true), onError);
    await eventually(() => expect(byLabel("Allowed senders")).not.toBeNull());

    await changeText(byLabel<HTMLTextAreaElement>("Allowed senders")!, "not-an-email");
    await click(findButton("Save settings")!);

    await eventually(() => expect(onError).toHaveBeenCalledWith("invalid_sender_rule"));
    expect(container?.querySelector('[role="alert"]')?.textContent).toContain(
      "Each allowed sender must be an email address or @domain without spaces.",
    );
    expect(byLabel<HTMLTextAreaElement>("Allowed senders")?.value).toBe("not-an-email");
    expect(findButton("Save settings")?.disabled).toBe(false);
  });

  it("displays Configured in .env and Managed via environment for env-managed configuration", async () => {
    server.use(
      http.get("/api/v1/settings/email", () => HttpResponse.json({
        enabled: true,
        source: "env",
        has_secret: true,
        secret_source: "env",
        secret_prefix: null,
        endpoint_url: "http://localhost/api/v1/ingest/email",
        user_id: null,
        allowed_senders: ["@example.com"],
        body_fallback: true,
        local_only: false,
        tag: null,
        last_received_at: "2026-02-10T12:00:00Z",
        received_count: 3,
      })),
    );

    await renderSection();
    await eventually(() => expect(container?.textContent).toContain("Configured in .env"));
    expect(container?.textContent).toContain("Managed via environment");
    expect(container?.textContent).toContain("3 emails received");
  });

  it("hides itself on 403 and formats Russian plural email counts", async () => {
    server.use(
      http.get("/api/v1/settings/email", () => HttpResponse.json({ code: "admin_required", message: "Forbidden" }, { status: 403 })),
    );
    await renderSection();
    await eventually(() => expect(container?.querySelector("section")).toBeNull());

    server.resetHandlers();
    for (const [count, expected] of [
      [0, "Получено 0 писем"],
      [1, "Получено 1 письмо"],
      [2, "Получено 2 письма"],
      [5, "Получено 5 писем"],
    ] as const) {
      server.use(
        http.get("/api/v1/settings/email", () => HttpResponse.json({
          enabled: true,
          source: "database",
          has_secret: true,
          secret_source: "database",
          secret_prefix: "pje_12345678",
          endpoint_url: "http://localhost/api/v1/ingest/email",
          user_id: null,
          allowed_senders: [],
          body_fallback: false,
          local_only: false,
          tag: null,
          last_received_at: null,
          received_count: count,
        })),
      );
      if (root) await act(async () => root?.unmount());
      container?.remove();
      container = document.createElement("div");
      document.body.append(container);
      root = createRoot(container);
      const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
      await act(async () => {
        root?.render(
          <QueryClientProvider client={queryClient}>
            <I18nextProvider i18n={createI18n("ru")}>
              <Harness confirm={vi.fn().mockResolvedValue(true)} />
            </I18nextProvider>
          </QueryClientProvider>,
        );
      });
      await eventually(() => expect(container?.textContent).toContain(expected));
    }
  });
});
