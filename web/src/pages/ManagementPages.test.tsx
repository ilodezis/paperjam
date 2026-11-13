import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { I18nextProvider } from "react-i18next";
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { http, HttpResponse } from "msw";
import { setupServer } from "msw/node";

import { api } from "../api";
import { createI18n } from "../i18n";
import { documentViews } from "../mocks/data";
import { handlers } from "../mocks/handlers";
import { ScreenHarness } from "../shell/testing";
import { SettingsPage } from "./SettingsPage";
import { TrashPage } from "./TrashPage";

const server = setupServer(...handlers);
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root | null = null;
let container: HTMLDivElement | null = null;

beforeAll(() => server.listen());
afterEach(async () => {
  server.resetHandlers();
  vi.restoreAllMocks();
  window.localStorage.clear();
  if (root) await act(async () => root?.unmount());
  container?.remove();
  root = null;
  container = null;
});
afterAll(() => server.close());

async function render(page: React.ReactNode, assertion: () => void | Promise<void>) {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  const i18n = createI18n("en");
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  await act(async () => {
    root?.render(
      <QueryClientProvider client={queryClient}>
        <I18nextProvider i18n={i18n}><ScreenHarness>{page}</ScreenHarness></I18nextProvider>
      </QueryClientProvider>
    );
  });
  await eventually(assertion);
}

function byLabel<T extends HTMLElement = HTMLElement>(label: string): T | null {
  return (Array.from(container?.querySelectorAll<HTMLElement>("[aria-label]") ?? [])
    .find((element) => element.getAttribute("aria-label") === label) as T | undefined) ?? null;
}

function button(name: string): HTMLButtonElement {
  const match = Array.from(container?.querySelectorAll("button") ?? []).find((element) => element.textContent?.trim() === name || element.getAttribute("aria-label") === name);
  if (!match) throw new Error(`Button not found: ${name}`);
  return match;
}

async function click(element: Element) {
  await act(async () => { element.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
}

async function change(element: HTMLElement, value: string) {
  const prototype = element instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
  Object.getOwnPropertyDescriptor(prototype, "value")?.set?.call(element, value);
  await act(async () => { element.dispatchEvent(new Event(element instanceof HTMLSelectElement ? "change" : "input", { bubbles: true })); });
}

async function eventually(assertion: () => void | Promise<void>) {
  let lastError: unknown;
  for (let attempt = 0; attempt < 40; attempt += 1) {
    try {
      await act(async () => { await assertion(); });
      return;
    } catch (error) {
      lastError = error;
      await act(async () => { await new Promise((resolve) => setTimeout(resolve, 10)); });
    }
  }
  throw lastError;
}

describe("TrashPage", () => {
  it("shows empty state with no empty trash button or notice when trash is empty", async () => {
    await render(<TrashPage />, () => expect(container?.textContent).toContain("Trash is empty"));
    expect(container?.querySelector(".trash-notice")).toBeNull();
    const emptyTrashBtn = Array.from(container?.querySelectorAll("header button") ?? []).find((b) => b.textContent?.includes("Empty trash"));
    expect(emptyTrashBtn).toBeUndefined();
  });

  it("lists only trashed documents and restores one with primary styling", async () => {
    await api.trashDocument("doc-receipt");
    await render(<TrashPage />, () => expect(byLabel("Restore Vacuum cleaner receipt")).not.toBeNull());
    expect(container?.textContent).not.toContain("Passport — Alex Example");

    const restoreBtn = byLabel("Restore Vacuum cleaner receipt");
    expect(restoreBtn?.classList.contains("button--secondary")).toBe(true);
    expect(restoreBtn?.classList.contains("button--primary")).toBe(false);
    const purgeBtn = byLabel("Permanently delete Vacuum cleaner receipt");
    expect(purgeBtn?.classList.contains("trash-purge")).toBe(true);

    expect(container?.querySelector(".trash-notice")).not.toBeNull();
    expect(button("Empty trash")).not.toBeNull();

    await click(restoreBtn!);
    await eventually(() => expect(byLabel("Restore Vacuum cleaner receipt")).toBeNull());
    expect((await api.document("doc-receipt")).trashed_at).toBeNull();
  });

  it("shows the thumbnail of a trashed document that has pages and none for one without", async () => {
    await api.trashDocument("doc-receipt");
    await api.trashDocument("doc-medical");
    server.use(http.get("/api/v1/documents/:id", ({ params }) => {
      const view = structuredClone(documentViews[String(params.id)]);
      if (params.id === "doc-medical") view.file.has_pages = false;
      return HttpResponse.json(view);
    }));
    try {
      await render(<TrashPage />, () => expect(byLabel("Restore Clinic visit summary")).not.toBeNull());
      const sources = Array.from(container?.querySelectorAll(".trash-document-mark img") ?? []).map((img) => img.getAttribute("src"));
      expect(sources).toEqual(["/api/v1/documents/doc-receipt/thumbnail.webp"]);
    } finally {
      await api.restoreDocument("doc-receipt");
      await api.restoreDocument("doc-medical");
    }
  });

  it("purges a document with confirmation that does not repeat title in description", async () => {
    await api.trashDocument("doc-medical");
    await render(<TrashPage />, () => expect(byLabel("Permanently delete Clinic visit summary")).not.toBeNull());

    await click(byLabel("Permanently delete Clinic visit summary")!);

    await eventually(() => expect(container?.querySelector(".confirm-dialog")).not.toBeNull());
    const titleEl = container?.querySelector("#confirm-dialog-title");
    const descEl = container?.querySelector("#confirm-dialog-desc");
    expect(titleEl?.textContent).toContain("Clinic visit summary");
    expect(descEl?.textContent).not.toContain("Clinic visit summary");
    expect(descEl?.textContent).toBe("The original file and all extracted data will be removed. This cannot be undone.");

    const confirmBtn = container?.querySelector(".confirm-actions .button--danger");
    expect(confirmBtn).not.toBeNull();
    await click(confirmBtn!);

    await eventually(() => expect(byLabel("Permanently delete Clinic visit summary")).toBeNull());
    await expect(api.document("doc-medical")).rejects.toMatchObject({ status: 404 });
  });

  it("empties the whole trash after a destructive confirmation", async () => {
    await api.trashDocument("doc-insurance");
    await render(<TrashPage />, () => expect(button("Empty trash").disabled).toBe(false));
    vi.spyOn(window, "confirm").mockReturnValue(true);

    await click(button("Empty trash"));
    await eventually(() => expect(container?.textContent).toContain("Trash is empty"));
    expect((await api.search({ query: "", includeTrashed: true })).hits.some((hit) => hit.id === "doc-insurance")).toBe(false);
  });
});

function tab(name: string): HTMLButtonElement {
  const match = Array.from(container?.querySelectorAll<HTMLButtonElement>('[role="tab"]') ?? []).find((element) => element.textContent?.trim() === name);
  if (!match) throw new Error(`Tab not found: ${name}`);
  return match;
}

function panel(name: string): HTMLElement {
  return container!.querySelector<HTMLElement>(`#settings-panel-${name}`)!;
}

async function key(element: Element, name: string) {
  await act(async () => { element.dispatchEvent(new KeyboardEvent("keydown", { key: name, bubbles: true, cancelable: true })); });
}

describe("SettingsPage", () => {
  it("splits settings into interface, archive and access, one visible at a time", async () => {
    await render(<SettingsPage />, () => expect(container?.querySelectorAll('[role="tab"]').length).toBe(3));
    expect(Array.from(container!.querySelectorAll('[role="tab"]')).map((element) => element.textContent)).toEqual(["Interface", "Archive", "Access"]);
    expect(tab("Interface").getAttribute("aria-selected")).toBe("true");
    expect(panel("interface").hidden).toBe(false);
    expect(panel("archive").hidden).toBe(true);
    expect(panel("access").hidden).toBe(true);
    // Personal preferences live apart from the statistics and the access lists.
    expect(panel("interface").textContent).toContain("Color theme");
    expect(panel("interface").textContent).not.toContain("LLM spend this month");
    expect(panel("interface").textContent).not.toContain("Reading device");

    await click(tab("Access"));
    expect(tab("Access").getAttribute("aria-selected")).toBe("true");
    expect(panel("access").hidden).toBe(false);
    expect(panel("interface").hidden).toBe(true);
    expect(panel("access").getAttribute("aria-labelledby")).toBe(tab("Access").id);
    expect(tab("Access").getAttribute("aria-controls")).toBe(panel("access").id);
  });

  it("moves between tabs with the arrow, Home and End keys", async () => {
    await render(<SettingsPage />, () => expect(container?.querySelectorAll('[role="tab"]').length).toBe(3));
    // Only the selected tab is in the Tab order.
    expect(tab("Interface").tabIndex).toBe(0);
    expect(tab("Archive").tabIndex).toBe(-1);

    tab("Interface").focus();
    await key(tab("Interface"), "ArrowRight");
    expect(tab("Archive").getAttribute("aria-selected")).toBe("true");
    expect(document.activeElement).toBe(tab("Archive"));
    await key(tab("Archive"), "End");
    expect(tab("Access").getAttribute("aria-selected")).toBe("true");
    await key(tab("Access"), "ArrowRight");
    expect(tab("Interface").getAttribute("aria-selected")).toBe("true");
    await key(tab("Interface"), "ArrowLeft");
    expect(tab("Access").getAttribute("aria-selected")).toBe("true");
    await key(tab("Access"), "Home");
    expect(tab("Interface").getAttribute("aria-selected")).toBe("true");
    expect(document.activeElement).toBe(tab("Interface"));
  });

  it("shows archive statistics under the archive tab", async () => {
    await render(<SettingsPage />, () => expect(tab("Archive")).not.toBeNull());
    await click(tab("Archive"));
    await eventually(() => expect(panel("archive").textContent).toContain("$0.04"));
    expect(panel("archive").textContent).toContain("Processing queue");
  });

  it("shows document types as a compact read-only list until customization is requested", async () => {
    await render(<SettingsPage />, () => expect(tab("Archive")).not.toBeNull());
    await click(tab("Archive"));
    await eventually(() => expect(byLabel("Customize fields for Receipt")).not.toBeNull());

    expect(panel("archive").textContent).toContain("Document types");
    expect(panel("archive").textContent).toContain("In use");
    expect(panel("archive").textContent).toContain("All");
    expect(panel("archive").textContent).toContain("2 fields");
    expect(byLabel("Field key 1")).toBeNull();
    expect(Array.from(panel("archive").querySelectorAll("button")).some((element) => element.textContent === "Add field")).toBe(false);
  });

  it("opens schema editing as a separate view and hides technical details until expert mode", async () => {
    await render(<SettingsPage />, () => expect(tab("Archive")).not.toBeNull());
    await click(tab("Archive"));
    await eventually(() => expect(byLabel("Customize fields for Receipt")).not.toBeNull());
    await click(byLabel("Customize fields for Receipt")!);

    expect(container?.querySelector(".document-type-list")).toBeNull();
    expect(byLabel("Back to document types")).not.toBeNull();
    expect(byLabel("Field key 1")).toBeNull();
    expect(byLabel("Field kind 1")).toBeNull();
    expect(byLabel<HTMLButtonElement>("Save Receipt")?.disabled).toBe(true);

    await change(byLabel("English field label 1")!, "Seller");
    expect(panel("archive").textContent).toContain("Unsaved changes");
    expect(byLabel<HTMLButtonElement>("Save Receipt")?.disabled).toBe(false);

    await click(byLabel("Expert mode")!);
    expect(byLabel("Field key 1")).not.toBeNull();
    expect(byLabel("Field kind 1")).not.toBeNull();
    await change(byLabel("Field key 1")!, "seller");
    await click(byLabel("Save Receipt")!);

    await eventually(async () => {
      const receipt = (await api.taxonomy()).types.find((type) => type.id === "type-receipt");
      expect(receipt?.fields[0]).toMatchObject({ key: "seller", labels: { en: "Seller", ru: "Продавец" } });
    });
    expect(byLabel<HTMLButtonElement>("Save Receipt")?.disabled).toBe(true);
  });

  it("adds a localized required field through the advanced schema editor", async () => {
    await render(<SettingsPage />, () => expect(tab("Archive")).not.toBeNull());
    await click(tab("Archive"));
    await eventually(() => expect(byLabel("Customize fields for Receipt")).not.toBeNull());
    await click(byLabel("Customize fields for Receipt")!);
    await click(button("Add field"));
    await click(byLabel("Expert mode")!);
    await change(byLabel("Field key 3")!, "warranty_until");
    await change(byLabel("English field label 3")!, "Warranty until");
    await change(byLabel("Russian field label 3")!, "Гарантия до");
    await click(byLabel("Field kind 3")!);
    await click(Array.from(container?.querySelectorAll<HTMLElement>('[role="option"]') ?? []).find((node) => node.textContent === "Date")!);
    await click(byLabel("Field required 3")!);
    await click(byLabel("Save Receipt")!);

    await eventually(async () => {
      const value = await api.taxonomy();
      expect(value.types.find((type) => type.id === "type-receipt")?.fields[2]).toMatchObject({ key: "warranty_until", kind: "date", required: true, labels: { en: "Warranty until", ru: "Гарантия до" } });
    });
  });

  it("marks a text field as the issuer and saves the flag, but offers no issuer flag for other kinds", async () => {
    await render(<SettingsPage />, () => expect(tab("Archive")).not.toBeNull());
    await click(tab("Archive"));
    await eventually(() => expect(byLabel("Customize fields for Receipt")).not.toBeNull());
    await click(byLabel("Customize fields for Receipt")!);

    expect(byLabel("Field names the issuer 2")).toBeNull();
    expect(byLabel("Field names the issuer 1")).not.toBeNull();
    expect(byLabel<HTMLInputElement>("Field names the issuer 1")?.checked).toBe(false);
    await click(byLabel("Field names the issuer 1")!);
    expect(byLabel<HTMLInputElement>("Field names the issuer 1")?.checked).toBe(true);
    await click(byLabel("Save Receipt")!);

    await eventually(async () => {
      const receipt = (await api.taxonomy()).types.find((type) => type.id === "type-receipt");
      expect(receipt?.fields[0]).toMatchObject({ kind: "text", issuer: true });
      expect(receipt?.fields[1]).not.toHaveProperty("issuer", true);
    });
  });

  it("creates a custom type without showing field definitions and filters unused types", async () => {
    await render(<SettingsPage />, () => expect(tab("Archive")).not.toBeNull());
    await click(tab("Archive"));
    await eventually(() => expect(byLabel("Customize fields for Receipt")).not.toBeNull());
    await click(button("Add document type"));

    expect(container?.querySelector(".field-definition-list")).toBeNull();
    expect(byLabel("New document type key")).toBeNull();
    await change(byLabel("New document type English label")!, "Warranty card");
    await change(byLabel("New document type Russian label")!, "Гарантийный талон");
    await click(button("Create document type"));

    await eventually(() => expect(byLabel("Customize fields for Warranty card")).not.toBeNull());
    expect((await api.taxonomy()).types.find((type) => type.key === "warranty_card")).toMatchObject({ labels: { en: "Warranty card", ru: "Гарантийный талон" }, fields: [] });
    await click(byLabel("In use")!);
    expect(byLabel("Customize fields for Warranty card")).toBeNull();
    await click(byLabel("All")!);
    expect(byLabel("Customize fields for Warranty card")).not.toBeNull();
  });

  it("keeps schema edits dirty when saving fails", async () => {
    server.use(http.patch("/api/v1/types/type-receipt", () => HttpResponse.json({ code: "network", message: "Synthetic failure" }, { status: 500 })));
    await render(<SettingsPage />, () => expect(tab("Archive")).not.toBeNull());
    await click(tab("Archive"));
    await eventually(() => expect(byLabel("Customize fields for Receipt")).not.toBeNull());
    await click(byLabel("Customize fields for Receipt")!);
    await change(byLabel("English field label 1")!, "Shop");
    await click(byLabel("Save Receipt")!);

    await eventually(() => expect(panel("archive").querySelector('[role="alert"]')?.textContent).toContain("The server could not be reached"));
    expect(byLabel<HTMLInputElement>("English field label 1")?.value).toBe("Shop");
    expect(panel("archive").textContent).toContain("Unsaved changes");
  });

  it("keeps the user form closed until its button is pressed, then creates a user", async () => {
    await render(<SettingsPage />, () => expect(tab("Access")).not.toBeNull());
    await click(tab("Access"));
    await eventually(() => expect(panel("access").textContent).toContain("Demo user"));
    expect(byLabel("New username")).toBeNull();

    await click(button("Add a user"));
    expect(document.activeElement).toBe(byLabel("New username"));
    await change(byLabel("New username")!, "sam");
    await change(byLabel("New display name")!, "Sam Example");
    await change(byLabel("New password")!, "correct horse");
    await click(button("Create user"));

    await eventually(() => expect(container?.textContent).toContain("Sam Example"));
    expect((await api.users()).find((user) => user.username === "sam")).toMatchObject({ locale: "en", is_admin: false });
    // The form closes after a successful create and the button is back.
    expect(byLabel("New username")).toBeNull();
    expect(button("Add a user")).not.toBeNull();

    // Admin can disable, re-enable, reset password, and delete the created user, while self has no destructive buttons.
    expect(byLabel("Delete Demo user")).toBeNull();
    vi.spyOn(window, "confirm").mockReturnValue(true);
    await click(byLabel("Disable Sam Example")!);
    await eventually(() => expect(panel("access").textContent).toContain("Disabled"));

    await click(byLabel("Enable Sam Example")!);
    await eventually(() => expect(panel("access").textContent).not.toContain("Disabled"));

    await click(byLabel("Reset password for Sam Example")!);
    const resetInput = panel("access").querySelector<HTMLInputElement>("#reset-user-password")!;
    await change(resetInput, "another secret 9");
    await click(button("Save password")!);
    await eventually(() => expect(panel("access").querySelector("#settings-reset-password-form")).toBeNull());

    await click(byLabel("Delete Sam Example")!);
    await eventually(() => expect(panel("access").textContent).not.toContain("Sam Example"));
  });

  it("lets any signed-in user change their own password", async () => {
    server.use(http.get("/api/v1/users", () => HttpResponse.json({ code: "forbidden", message: "Administrators only" }, { status: 403 })));
    const changePassword = vi.spyOn(api, "changePassword");
    await render(<SettingsPage />, () => expect(tab("Access")).not.toBeNull());
    await click(tab("Access"));
    await eventually(() => expect(panel("access").textContent).toContain("Change password"));

    await click(button("Change password"));
    await change(byLabel("Current password")!, "old password 1");
    await change(byLabel("New password")!, "new password 2");
    await change(byLabel("Confirm new password")!, "mismatch 3");
    await click(button("Save password"));
    await eventually(() => expect(container?.textContent).toContain("The new passwords do not match."));
    expect(changePassword).not.toHaveBeenCalled();

    await change(byLabel("Confirm new password")!, "new password 2");
    await click(button("Save password"));
    await eventually(() => expect(changePassword).toHaveBeenCalledWith("old password 1", "new password 2"));
    expect(byLabel("Current password")).toBeNull();
  });

  it("cancels the user form without creating anyone and returns focus to its button", async () => {
    await render(<SettingsPage />, () => expect(tab("Access")).not.toBeNull());
    await click(tab("Access"));
    await eventually(() => expect(panel("access").textContent).toContain("Demo user"));
    await click(button("Add a user"));
    await change(byLabel("New username")!, "draft");
    await click(Array.from(panel("access").querySelectorAll("#settings-user-form button")).find((element) => element.textContent === "Cancel")!);

    expect(byLabel("New username")).toBeNull();
    expect(document.activeElement).toBe(button("Add a user"));
    expect((await api.users()).some((user) => user.username === "draft")).toBe(false);
  });

  it("reveals a new token once and revokes it", async () => {
    await render(<SettingsPage />, () => expect(tab("Access")).not.toBeNull());
    await click(tab("Access"));
    await eventually(() => expect(container?.textContent).toContain("Reading device"));
    expect(byLabel("New token name")).toBeNull();

    await click(button("New token"));
    await change(byLabel("New token name")!, "Laptop");
    await click(button("Create token"));

    await eventually(() => expect((byLabel<HTMLInputElement>("New token secret")?.value)).toMatch(/^pj_/));
    expect(container?.textContent).toContain("Copy it now");
    expect(byLabel("New token name")).toBeNull();

    // Switching tabs does not lose a secret that is shown only once.
    await click(tab("Interface"));
    await click(tab("Access"));
    expect(byLabel<HTMLInputElement>("New token secret")?.value).toMatch(/^pj_/);

    vi.spyOn(window, "confirm").mockReturnValue(true);
    await click(byLabel("Revoke Laptop")!);
    await eventually(() => expect(container?.textContent).toContain("Revoked"));
    expect((await api.tokens()).find((token) => token.name === "Laptop")?.revoked_at).not.toBeNull();
  });

  it("shows the calendar link once, rotates it and revokes it", async () => {
    await render(<SettingsPage />, () => expect(tab("Access")).not.toBeNull());
    await click(tab("Access"));
    await eventually(() => expect(panel("access").textContent).toContain("Calendar subscription"));
    expect(panel("access").textContent).toContain("Google Calendar fetches the link from Google's servers");
    expect(byLabel("Calendar link")).toBeNull();

    await eventually(() => expect(button("Create link")).not.toBeNull());
    await click(button("Create link"));
    await eventually(() => expect(byLabel<HTMLInputElement>("Calendar link")?.value).toMatch(/\/calendar\/pjc_[0-9a-f]{32}\.ics$/));
    const first = byLabel<HTMLInputElement>("Calendar link")!.value;
    await eventually(() => expect(panel("access").textContent).toContain("Link active"));

    vi.spyOn(window, "confirm").mockReturnValue(true);
    await click(button("New link"));
    await eventually(() => expect(byLabel<HTMLInputElement>("Calendar link")?.value).not.toBe(first));
    expect(byLabel<HTMLInputElement>("Calendar link")?.value).toMatch(/\/calendar\/pjc_/);

    await click(byLabel("Revoke calendar link")!);
    await eventually(() => expect(byLabel("Calendar link")).toBeNull());
    expect((await api.calendarFeed()).active).toBe(false);
    expect(button("Create link")).not.toBeNull();
  });

  it("keeps connected apps and passkeys under access and disconnects an app after confirmation", async () => {
    const disconnect = vi.spyOn(api, "disconnect");
    await render(<SettingsPage />, () => expect(tab("Access")).not.toBeNull());
    await click(tab("Access"));
    await eventually(() => expect(panel("access").textContent).toContain("Assistant"));
    expect(panel("access").textContent).toContain("Connected apps");
    await eventually(() => expect(panel("access").textContent).toContain("Demo phone"));
    expect(panel("access").textContent).toContain("Passkeys");

    vi.spyOn(window, "confirm").mockReturnValue(true);
    await click(byLabel("Disconnect Assistant")!);
    await eventually(() => expect(disconnect).toHaveBeenCalledWith("grant-assistant"));
    await eventually(() => expect(container?.textContent).toContain("Nothing is connected yet."));
  });

  it("shows access without the user list to someone who is not an administrator", async () => {
    server.use(http.get("/api/v1/users", () => HttpResponse.json({ code: "forbidden", message: "Administrators only" }, { status: 403 })));
    await render(<SettingsPage />, () => expect(tab("Access")).not.toBeNull());
    await click(tab("Access"));
    await eventually(() => expect(panel("access").textContent).toContain("Reading device"));
    expect(panel("access").textContent).not.toContain("Add a user");
    expect(panel("access").querySelector("#settings-users")).toBeNull();
    expect(panel("access").textContent).toContain("API tokens");
    expect(panel("access").textContent).toContain("Connected apps");
    expect(panel("access").textContent).toContain("Passkeys");
  });

  it("changes and remembers the interface language", async () => {
    const setLocale = vi.spyOn(api, "setLocale");
    await render(<SettingsPage />, () => expect(byLabel("Settings interface language")).not.toBeNull());
    await change(byLabel("Settings interface language")!, "ru");

    await eventually(() => expect(container?.textContent).toContain("Доступ"));
    expect(window.localStorage.getItem("paperjam:language")).toBe("ru");
    expect(document.documentElement.lang).toBe("ru");
    // The account keeps it too: summaries of new documents follow it.
    await eventually(() => expect(setLocale).toHaveBeenCalledWith("ru"));
  });
});
