import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { I18nextProvider } from "react-i18next";
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { http, HttpResponse } from "msw";
import { setupServer } from "msw/node";

import { ApiError, api, type DocumentView } from "../api";
import { ToastHost } from "../components/controls/ToastHost";
import { clearToasts } from "../components/controls/toast";
import { createI18n } from "../i18n";
import { documentViews, taxonomy } from "../mocks/data";
import { handlers } from "../mocks/handlers";
import { ScreenHarness } from "../shell/testing";
import { DocumentPage, failureReasonKey, humanizeKey } from "./DocumentPage";

const server = setupServer(...handlers);
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root | null = null;
let container: HTMLDivElement | null = null;

beforeAll(() => server.listen());
afterEach(async () => {
  server.resetHandlers();
  vi.restoreAllMocks();
  clearToasts();
  if (root) await act(async () => root?.unmount());
  container?.remove();
  root = null;
  container = null;
});
afterAll(() => server.close());

async function render(documentId: string, assertion: () => void | Promise<void>, language = "en") {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  const i18n = createI18n(language);
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  await act(async () => {
    root?.render(
      <QueryClientProvider client={queryClient}>
        <I18nextProvider i18n={i18n}>
          <ScreenHarness id="document" path={`/documents/${documentId}`}>
            <DocumentPage />
          </ScreenHarness>
          <ToastHost />
        </I18nextProvider>
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

/** Opens a custom select and clicks one of its options, the way a person does. */
async function choose(label: string, optionText: string) {
  await click(byLabel(label)!);
  const option = Array.from(container?.querySelectorAll('[role="option"]') ?? []).find((node) => node.textContent === optionText);
  if (!option) throw new Error(`Option not found: ${optionText}`);
  await click(option);
}

/** The text a custom select currently shows: the chosen value or its placeholder. */
function shown(label: string): string | null | undefined {
  return byLabel(label)?.querySelector(".select-dropdown-value")?.textContent;
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

describe("DocumentPage share", () => {
  beforeAll(() => {
    // jsdom has no Web Animations: the share sheet opens and closes at once.
    Element.prototype.animate = (() => ({ finished: Promise.resolve(), cancel: () => undefined })) as unknown as Element["animate"];
  });

  /** The share sheet lives in a portal, outside the page's container. */
  const sheet = () => document.querySelector<HTMLElement>(".share-sheet");
  const inSheet = (name: string) =>
    Array.from(sheet()?.querySelectorAll<HTMLElement>("button, [role=radio], [role=switch]") ?? []).find((element) =>
      element.textContent?.trim() === name || element.getAttribute("aria-label") === name,
    );

  it("opens the share sheet without making a link, then makes one with the chosen lifetime and details", async () => {
    const createShare = vi.spyOn(api, "createShare");
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText: vi.fn().mockResolvedValue(undefined) } });
    await render("doc-rental", () => expect(button("Share")).toBeTruthy());

    await click(button("Share"));
    await eventually(() => expect(sheet()?.textContent).toContain("No live links."));
    expect(createShare).not.toHaveBeenCalled();

    await click(inSheet("1 hour")!);
    await click(sheet()!.querySelector("[role=switch]")!);
    await click(inSheet("Create and copy link")!);

    await eventually(() => expect(sheet()?.textContent).toContain("Without details"));
    expect(createShare).toHaveBeenCalledTimes(1);
    expect(createShare).toHaveBeenCalledWith("doc-rental", { ttl: "1h", show_details: false });
    expect(navigator.clipboard.writeText).toHaveBeenCalledWith(expect.stringContaining("/s/share-"));
  });

  it("revokes a live link", async () => {
    const revokeShare = vi.spyOn(api, "revokeShare");
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText: vi.fn().mockResolvedValue(undefined) } });
    await render("doc-insurance", () => expect(button("Share")).toBeTruthy());

    await click(button("Share"));
    await eventually(() => expect(inSheet("Create and copy link")).toBeTruthy());
    await click(inSheet("Create and copy link")!);
    await eventually(() => expect(inSheet("Revoke link")).toBeTruthy());

    await click(inSheet("Revoke link")!);
    await eventually(() => expect(sheet()?.textContent).toContain("No live links."));
    expect(revokeShare).toHaveBeenCalledTimes(1);
  });

  it("hands off cleanly from the mobile Actions sheet to Share without overlapping scrims or stealing focus", async () => {
    await render("doc-rental", () => expect(byLabel("Actions")).not.toBeNull());
    const actionsBtn = byLabel("Actions")!;
    actionsBtn.focus();

    await click(actionsBtn);
    await eventually(() => expect(document.querySelector('[role="dialog"]')?.getAttribute("aria-label")).toBe("Actions"));

    const shareBtn = Array.from(document.querySelectorAll<HTMLButtonElement>(".sheet-actions button"))
      .find((el) => el.textContent?.trim() === "Share");
    expect(shareBtn).toBeTruthy();

    await click(shareBtn!);
    await eventually(() => expect(inSheet("Create and copy link")).toBeTruthy());
    expect(document.querySelectorAll('[role="dialog"]')).toHaveLength(1);
    expect(document.querySelectorAll(".scrim")).toHaveLength(1);
    expect(actionsBtn.closest("[inert]")).not.toBeNull();
    expect(document.querySelector('[role="dialog"]')?.contains(document.activeElement)).toBe(true);
  });
});

describe("DocumentPage download", () => {
  it("downloads the clean copy under the title, and offers the original file beside it", async () => {
    await render("doc-passport", () => expect(container?.querySelector(".document-header .eyebrow")).toBeTruthy());

    // The line over the title names the format and size, not the title again.
    expect(container?.querySelector(".document-header .eyebrow")?.textContent).toMatch(/^PDF · /);
    const download = Array.from(container?.querySelectorAll<HTMLAnchorElement>(".document-actions a") ?? []).find((a) => a.textContent?.includes("Download"));
    expect(download?.getAttribute("href")).toBe("/api/v1/documents/doc-passport/download");
    expect(download?.getAttribute("title")).toBe("Passport — Alex Example.pdf");
    const original = Array.from(container?.querySelectorAll<HTMLAnchorElement>(".action-menu a") ?? []).find((a) => a.textContent === "Original (as uploaded)");
    expect(original?.getAttribute("href")).toBe("/api/v1/documents/doc-passport/original");
  });

  it("offers no separate original when there is no clean copy", async () => {
    await render("doc-rental", () => expect(button("Share")).toBeTruthy());
    const original = Array.from(container?.querySelectorAll<HTMLAnchorElement>(".action-menu a") ?? []).find((a) => a.textContent === "Original (as uploaded)");
    expect(original).toBeUndefined();
  });

  it("shows a scan from its clean copy, and swaps in the original pages on request", async () => {
    await render("doc-passport", () => expect(container?.querySelector(".page-image")).toBeTruthy());
    const pageSrc = () => container?.querySelector(".page-image")?.getAttribute("src");
    expect(pageSrc()).toBe("/api/v1/documents/doc-passport/pages/1.webp");

    const toggle = byLabel("Show the pages as scanned (before cleaning)")!;
    expect(toggle.getAttribute("aria-pressed")).toBe("false");
    await click(toggle);
    expect(pageSrc()).toBe("/api/v1/documents/doc-passport/pages/1-original.webp");
    expect(toggle.getAttribute("aria-pressed")).toBe("true");
    await click(toggle);
    expect(pageSrc()).toBe("/api/v1/documents/doc-passport/pages/1.webp");
  });

  it("has no original-pages toggle when the document has no clean copy", async () => {
    await render("doc-rental", () => expect(button("Share")).toBeTruthy());
    expect(byLabel("Show the pages as scanned (before cleaning)")).toBeNull();
  });
});

function summary(name: string): HTMLElement {
  const match = Array.from(container?.querySelectorAll("summary") ?? []).find((element) => element.textContent?.trim() === name || element.getAttribute("aria-label") === name);
  if (!match) throw new Error(`Summary not found: ${name}`);
  return match;
}

async function openActions() {
  await click(summary("More actions"));
}

describe("DocumentPage without page images", () => {
  /** Serves the stock fixture of a document, changed by `change`, in place of the stock one. */
  function serve(change: (view: DocumentView) => DocumentView) {
    server.use(
      http.get("/api/v1/documents/:id", ({ params }) => {
        const view = documentViews[String(params.id)];
        if (!view) return HttpResponse.json({ code: "not_found", message: "Document not found" }, { status: 404 });
        return HttpResponse.json(change(structuredClone(view)));
      }),
    );
  }

  /** Any picture the page drew from a page render or a thumbnail. */
  const pageImageNodes = () => Array.from(container?.querySelectorAll("img") ?? []).filter((img) => /\/pages\/|thumbnail\.webp/.test(img.getAttribute("src") ?? ""));

  it("requests no page image for a failed document and shows the unavailable placeholder", async () => {
    serve((view) => ({ ...view, status: "failed", status_reason: null, text: null, file: { ...view.file, page_count: null, has_pages: false } }));
    await render("doc-rental", () => expect(container?.querySelector(".page-unavailable")?.textContent).toContain("No preview: the file could not be read."));
    expect(pageImageNodes()).toHaveLength(0);
    expect(container?.querySelector(".page-placeholder")).toBeNull();
  });

  it("requests no page image for a text-only document and shows the placeholder", async () => {
    serve((view) => ({ ...view, file: { ...view.file, page_count: 1, has_pages: false } }));
    await render("doc-rental", () => expect(container?.querySelector(".page-unavailable")?.textContent).toContain("The page preview is not available yet."));
    expect(pageImageNodes()).toHaveLength(0);
    expect(container?.querySelector(".page-placeholder")).toBeNull();
  });

  it("still asks for the first page of a document that has one", async () => {
    await render("doc-rental", () => expect(container?.querySelector(".page-image")).toBeTruthy());
    expect(container?.querySelector(".page-image")?.getAttribute("src")).toBe("/api/v1/documents/doc-rental/pages/1.webp");
    expect(container?.querySelector(".page-unavailable")).toBeNull();
  });
});

describe("DocumentPage actions", () => {
  it("puts secondary and destructive actions at the bottom of one menu", async () => {
    await render("doc-rental", () => expect(summary("More actions")).toBeTruthy());

    expect(container?.querySelector(".document-actions > .document-trash")).toBeNull();
    await openActions();
    const menu = container?.querySelector(".action-menu");
    expect(menu?.hasAttribute("open")).toBe(true);
    const actions = Array.from(menu?.querySelectorAll("button") ?? []);
    expect(actions.at(-1)?.getAttribute("aria-label")).toBe("Move to trash");
    expect(actions.at(-1)?.classList.contains("action-menu-danger")).toBe(true);
  });

  it("closes an open menu on Escape, focuses its summary and keeps the page", async () => {
    await render("doc-receipt", () => expect(summary("More actions")).toBeTruthy());
    await openActions();

    await act(async () => { document.body.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })); });

    expect(container?.querySelector(".action-menu")?.hasAttribute("open")).toBe(false);
    expect(document.activeElement).toBe(summary("More actions"));
    expect(container?.querySelector(".document-page")).not.toBeNull();
  });

  it("closes the action menu on a click outside it", async () => {
    await render("doc-receipt", () => expect(summary("More actions")).toBeTruthy());
    await openActions();

    await act(async () => { document.body.dispatchEvent(new Event("pointerdown", { bubbles: true })); });

    expect(container?.querySelector(".action-menu")?.hasAttribute("open")).toBe(false);
  });
});

describe("DocumentPage status", () => {
  it("shows needs attention in place of ready while attention is open", async () => {
    await render("doc-medical", () => expect(container?.querySelector(".document-status--attention")).not.toBeNull());

    expect(container?.querySelector(".document-status--attention")?.textContent).toContain("Needs attention");
    expect(container?.querySelector(".document-status--ready")).toBeNull();
  });

  it("maps failure codes to their own sentence and any other code to the generic one", () => {
    expect(failureReasonKey("pdf_encrypted")).toBe("pdf_encrypted");
    expect(failureReasonKey("something_new")).toBe("unknown");
  });

  it("shows a queued toast and processing spinner instead of needs attention when reprocessing starts", async () => {
    const reprocess = vi.spyOn(api, "reprocess");
    await render("doc-medical", () => expect(container?.querySelector(".document-status--attention")).not.toBeNull());

    await openActions();
    await click(button("Local extraction + LLM"));

    await eventually(() => expect(container?.textContent).toContain("Queued for reprocessing"));
    expect(reprocess).toHaveBeenCalledWith("doc-medical", true);
    const activeStatus = container?.querySelector(".document-status--queued, .document-status--processing");
    expect(activeStatus).not.toBeNull();
    expect(activeStatus?.querySelector(".spin")).not.toBeNull();
    expect(container?.querySelector(".document-status--attention")).toBeNull();
  });

  it("offers reprocess and dismiss on mrz_mismatch and clears it when dismissed", async () => {
    const base = await api.document("doc-passport");
    let current = { ...base, attention: ["mrz_mismatch"] };
    server.use(
      http.get("/api/v1/documents/doc-passport", () => HttpResponse.json(current)),
      http.patch("/api/v1/documents/doc-passport", async ({ request }) => {
        const body = await request.json() as { dismiss_attention?: string[] };
        if (body.dismiss_attention) {
          current = { ...current, attention: current.attention.filter((f) => !body.dismiss_attention?.includes(f)) };
        }
        return HttpResponse.json(current);
      }),
    );
    const patch = vi.spyOn(api, "patchDocument");
    await render("doc-passport", () => expect(container?.querySelector(".attention-section")).not.toBeNull());

    const actions = container!.querySelector(".attention-actions")!;
    expect(actions.textContent).toContain("Reprocess");
    expect(actions.textContent).toContain("Dismiss");
    await click(button("Dismiss"));

    await eventually(() => expect(patch).toHaveBeenCalledWith("doc-passport", { dismiss_attention: ["mrz_mismatch"] }));
    await eventually(() => expect(container?.querySelector(".attention-section")).toBeNull());
  });
});

describe("DocumentPage per-model reprocessing", () => {
  beforeAll(() => {
    Element.prototype.animate = (() => ({ finished: Promise.resolve(), cancel: () => undefined })) as unknown as Element["animate"];
  });

  it("shows model-specific reprocess buttons when multiple allowed models are configured and calls api.reprocess with the selected model", async () => {
    const reprocess = vi.spyOn(api, "reprocess");
    await render("doc-rental", () => expect(summary("More actions")).toBeTruthy());

    await openActions();
    await eventually(() => expect(button("anthropic/claude-sonnet-5.5")).toBeTruthy());
    expect(button("Local extraction + LLM")).toBeTruthy();
    expect(button("Local extraction only")).toBeTruthy();
    expect(button("anthropic/claude-haiku-5.5")).toBeTruthy();

    await click(button("anthropic/claude-sonnet-5.5"));
    await eventually(() => expect(reprocess).toHaveBeenCalledWith("doc-rental", true, "anthropic/claude-sonnet-5.5"));
    await eventually(() => expect(container?.textContent).toContain("Queued for reprocessing"));
  });

  it("offers per-model reprocess options in the mobile action sheet and hides them when only one model is allowed", async () => {
    const reprocess = vi.spyOn(api, "reprocess");
    await render("doc-rental", () => expect(byLabel("Actions")).not.toBeNull());

    await click(byLabel("Actions")!);
    await eventually(() => expect(document.querySelector(".sheet")).not.toBeNull());
    const sheetBtn = Array.from(document.querySelectorAll<HTMLButtonElement>(".sheet button"))
      .find((el) => el.textContent?.trim() === "anthropic/claude-haiku-5.5");
    expect(sheetBtn).toBeTruthy();
    await click(sheetBtn!);
    await eventually(() => expect(reprocess).toHaveBeenCalledWith("doc-rental", true, "anthropic/claude-haiku-5.5"));

    await act(async () => root?.unmount());
    container?.remove();

    server.use(
      http.get("/api/v1/settings/summary", () => HttpResponse.json({
        llm_enabled: true,
        llm_configured: true,
        llm_model: "anthropic/claude-haiku-5.5",
        llm_handwriting_enabled: true,
        llm_allowed_models: ["anthropic/claude-haiku-5.5"],
        llm_monthly_budget_usd: "5.00",
        calendar_lead_days: 60,
        inbox_configured: false,
        inbox_paused: false,
      })),
    );
    await render("doc-rental", () => expect(summary("More actions")).toBeTruthy());
    await openActions();
    expect(container?.querySelector(".action-menu-subitem")).toBeNull();
  });
});

describe("DocumentPage trash", () => {
  it("moves to trash without a confirm dialog and offers Undo in the toast", async () => {
    await render("doc-receipt", () => expect(summary("More actions")).toBeTruthy());

    await openActions();
    await click(byLabel("Move to trash")!);
    await eventually(() => expect(container?.textContent).toContain("Document moved to trash"));
    expect(container?.querySelector('[role="alertdialog"]')).toBeNull();
    expect(button("Undo")).toBeTruthy();
    expect((await api.document("doc-receipt")).trashed_at).not.toBeNull();
  });

  it("restores the document when Undo is clicked", async () => {
    const restore = vi.spyOn(api, "restoreDocument");
    await render("doc-receipt", () => expect(summary("More actions")).toBeTruthy());

    await openActions();
    await click(byLabel("Move to trash")!);
    await eventually(() => expect(button("Undo")).toBeTruthy());
    await click(button("Undo"));

    await eventually(() => expect(restore).toHaveBeenCalledWith("doc-receipt"));
    await eventually(async () => expect((await api.document("doc-receipt")).trashed_at).toBeNull());
  });
});

describe("DocumentPage metadata", () => {
  it("shows the description and other facts in place and keeps extraction sources on demand", async () => {
    await render("doc-rental", () => expect(container?.textContent).toContain("General"));

    const headings = Array.from(container?.querySelectorAll(".document-metadata > .metadata-section > .metadata-section-heading > h2, .document-metadata > .metadata-section > h2") ?? []).map((heading) => heading.textContent);
    expect(headings).toEqual(["General", "Description", "Privacy and processing", "Additional fields"]);
    expect(container?.querySelectorAll(".provenance")).toHaveLength(0);

    expect(container?.querySelector(".description-editor")).toBeNull();
    expect(container?.querySelector(".description-text")?.textContent).toContain("The agreement begins");
    expect(container?.querySelector(".description-field")).toBeNull();
    await click(container!.querySelector(".description-edit")!);
    expect(container?.querySelector<HTMLTextAreaElement>("textarea.description-field")?.value).toContain("The agreement begins");

    const processing = container?.querySelector<HTMLDetailsElement>(".processing-details");
    expect(processing?.open).toBe(false);
    await click(processing!.querySelector("summary")!);
    expect(processing?.textContent).toContain("LLM");
    expect(processing?.textContent).toContain("Title");

    expect(container?.querySelector(".facts-disclosure")).toBeNull();
    const facts = container?.querySelector<HTMLElement>("section.facts-section");
    expect(facts?.querySelector("h3")?.textContent).toBe("Other facts");
    expect(facts?.querySelector("dl.facts-list")).not.toBeNull();
    expect(facts?.textContent).toContain("Archive reference");
  });

  it("saves an edited description when focus leaves the field", async () => {
    const patch = vi.spyOn(api, "patchDocument");
    await render("doc-rental", () => expect(container?.querySelector(".description-edit")).not.toBeNull());

    await click(container!.querySelector(".description-edit")!);
    const field = container!.querySelector<HTMLTextAreaElement>("textarea.description-field")!;
    await act(async () => {
      field.focus();
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set?.call(field, "Short new note");
    });
    expect(patch).not.toHaveBeenCalled();
    await act(async () => { field.blur(); });

    await eventually(() => expect(patch).toHaveBeenCalledWith("doc-rental", { summary: "Short new note" }));
  });

  it("groups typed fields by their meaning", async () => {
    await render("doc-rental", () => expect(container?.querySelector(".typed-field-groups")).not.toBeNull());

    const groups = Array.from(container?.querySelectorAll(".typed-field-groups h3") ?? []).map((heading) => heading.textContent);
    expect(groups).toEqual(["Dates", "Amounts"]);
  });

  it("localizes fact labels without a field definition", async () => {
    await render("doc-rental", () => expect(container?.querySelector(".facts-section")).not.toBeNull(), "ru");

    expect(container?.querySelector(".facts-section")?.textContent).toContain("Архивная ссылка");
    expect(container?.querySelector(".facts-section")?.textContent).not.toContain("Archive reference");
  });

  it("names a fact by the model's label rather than its raw key", async () => {
    await render("doc-rental", () => expect(container?.querySelector(".facts-section")).not.toBeNull(), "ru");

    const facts = container?.querySelector(".facts-section")?.textContent ?? "";
    expect(facts).toContain("Снят с регистрационного учёта");
    expect(facts).not.toContain("Registration removed");
  });

  it("uses concise page references with full accessible names", async () => {
    await render("doc-rental", () => expect(byLabel("Go to page 1")).not.toBeNull());

    expect(byLabel("Go to page 1")?.textContent).toBe("p. 1");
  });

  it("scrolls the viewer into view when tapping a page reference while the viewer is off-screen", async () => {
    await render("doc-rental", () => expect(byLabel("Go to page 2")).not.toBeNull());
    const viewer = container!.querySelector<HTMLElement>(".document-viewer")!;
    const scrollIntoView = vi.fn();
    viewer.scrollIntoView = scrollIntoView;

    // Simulate mobile scroll where the viewer is above the viewport.
    viewer.getBoundingClientRect = () => ({ top: -400, bottom: -50, left: 0, right: 390, width: 390, height: 350, x: 0, y: -400, toJSON: () => ({}) }) as DOMRect;
    await click(byLabel("Go to page 2")!);
    expect(scrollIntoView).toHaveBeenCalledWith({ behavior: "smooth", block: "start" });

    // When the viewer is already in view (e.g. sticky desktop column), do not jump scroll.
    scrollIntoView.mockClear();
    viewer.getBoundingClientRect = () => ({ top: 16, bottom: 600, left: 0, right: 600, width: 600, height: 584, x: 0, y: 16, toJSON: () => ({}) }) as DOMRect;
    await click(byLabel("Go to page 1")!);
    expect(scrollIntoView).not.toHaveBeenCalled();
  });

  it("leaves out page references on a one-page document", async () => {
    await render("doc-receipt", () => expect(container?.querySelector(".fields-section, .metadata-section")).not.toBeNull());

    expect(container?.querySelector(".page-reference")).toBeNull();
  });
});

describe("DocumentPage date field", () => {
  it("names the date input once, from its own label, not from a wrapping label", async () => {
    await render("doc-rental", () => expect(container?.querySelector('input[type="date"]')).not.toBeNull());

    const input = container!.querySelector<HTMLInputElement>('input[type="date"]')!;
    expect(input.closest("label")).toBeNull();
    const labelledBy = input.getAttribute("aria-labelledby");
    expect(labelledBy).toBeTruthy();
    expect(document.getElementById(labelledBy!)?.textContent).toBe("Document date");
    expect(input.labels?.length ?? 0).toBe(0);
  });

  it("gives the date input a non-empty visible label in Russian, so its name is not built from the picker button", async () => {
    await render("doc-rental", () => expect(container?.querySelector('input[type="date"]')).not.toBeNull(), "ru");

    const input = container!.querySelector<HTMLInputElement>('input[type="date"]')!;
    const labelledBy = input.getAttribute("aria-labelledby");
    const labelElement = labelledBy ? document.getElementById(labelledBy) : null;
    expect(labelElement).not.toBeNull();
    expect(labelElement?.contains(input)).toBe(false);
    expect(labelElement?.textContent?.trim()).toBe("Дата документа");
  });
});

describe("DocumentPage fields", () => {
  it("names the field behind a flag and saves it unchanged when confirmed", async () => {
    const base = await api.document("doc-rental");
    server.use(http.get("/api/v1/documents/doc-rental", () => HttpResponse.json({
      ...base,
      attention: ["ocr_llm_disagree"],
      fields: base.fields.map((entry) => entry.key === "amount" ? { ...entry, issues: ["ocr_llm_disagree"] } : entry),
    })));
    const patch = vi.spyOn(api, "patchDocument");
    await render("doc-rental", () => expect(container?.textContent).toContain("Check: Monthly amount."));

    expect(container?.querySelector(".field-issue")?.textContent).toContain("not in the recognized text");
    await click(byLabel("The value of “Monthly amount” is correct")!);

    await eventually(() => expect(patch).toHaveBeenCalledWith("doc-rental", {
      fields: { amount: { kind: "money", value: { amount: "1250.00", currency: "EUR" } } },
    }));
  });

  it("saves a typed value when focus leaves the field, with no save button", async () => {
    const patch = vi.spyOn(api, "patchDocument");
    await render("doc-rental", () => expect(byLabel("Monthly amount")).not.toBeNull());
    expect(byLabel("Save field “Monthly amount”")).toBeNull();

    const input = byLabel<HTMLInputElement>("Monthly amount")!;
    await act(async () => {
      input.focus();
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(input, "1300");
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    expect(patch).not.toHaveBeenCalled();
    await act(async () => { input.blur(); });

    await eventually(() => expect(patch).toHaveBeenCalledWith("doc-rental", {
      fields: { amount: { kind: "money", value: { amount: "1300", currency: "EUR" } } },
    }));
  });

  it("labels keys the type does not name", () => {
    expect(humanizeKey("tax_id_inn")).toBe("Tax ID INN");
    expect(humanizeKey("archive_reference")).toBe("Archive reference");
  });

  it("displays correct empty labels for people, organizations, and tags in English", async () => {
    const base = await api.document("doc-rental");
    server.use(http.get("/api/v1/documents/doc-empty", () => HttpResponse.json({
      ...base,
      id: "doc-empty",
      organizations: [],
      people: [],
      tags: [],
    })));

    await render("doc-empty", () => expect(container?.textContent).toContain("Not specified"));
    const textEn = container?.textContent ?? "";
    expect(textEn).toContain("Not specified");
    expect(textEn).toContain("No tags selected");
    expect(textEn).not.toContain("None selected");
    expect(textEn).not.toContain("Nobody selected");
    expect(textEn).not.toContain("No correspondent");
  });

  it("displays correct empty labels for people, organizations, and tags in Russian", async () => {
    const base = await api.document("doc-rental");
    server.use(http.get("/api/v1/documents/doc-empty", () => HttpResponse.json({
      ...base,
      id: "doc-empty",
      organizations: [],
      people: [],
      tags: [],
    })));

    await render("doc-empty", () => expect(container?.textContent).toContain("Не указан"), "ru");
    const textRu = container?.textContent ?? "";
    // The empty lists are plural: not "Не указан" (singular), which was a grammar slip.
    expect(textRu).toContain("Не указаны");
    expect(textRu).not.toMatch(/Не указан(?!ы)/);
    expect(textRu).toContain("Метки не выбраны");
    expect(textRu).not.toContain("Никто не выбран");
  });

  it("renders long fact values with a concise page reference and a promote control", async () => {
    const base = await api.document("doc-rental");
    const longValue = "A long contractual clause that must wrap without covering its controls. ".repeat(4);
    server.use(http.get("/api/v1/documents/doc-facts", () => HttpResponse.json({
      ...base,
      id: "doc-facts",
      facts: [
        { key: "legal_note", value: longValue, provenance: "local", page: 2 },
      ],
    })));

    await render("doc-facts", () => expect(container?.querySelector(".facts-section")).not.toBeNull());
    const factValue = container?.querySelector(".fact-value");
    expect(factValue?.textContent).toBe(longValue);

    const pageRef = byLabel("Go to page 2");
    expect(pageRef?.textContent).toBe("p. 2");

    const promote = container?.querySelector(".facts-list .fact-promote");
    expect(promote).not.toBeNull();
    expect(promote?.querySelector('[role="combobox"]')).not.toBeNull();
  });

  it("hides raw MRZ lines after useful values have been extracted", async () => {
    const base = await api.document("doc-passport");
    const rawMrz = "P<UTOERIKSSON<<ANNA<MARIA<<<<<<<<<<<<<<<<<<<L898902C36UTO7408122F1204159ZE184226B<<<<<10";
    server.use(http.get("/api/v1/documents/doc-mrz", () => HttpResponse.json({
      ...base,
      id: "doc-mrz",
      facts: [
        { key: "mrz_line", value: rawMrz, provenance: "mrz", page: 1 },
        { key: "archive_reference", value: "DEMO-PASSPORT", provenance: "local", page: 1 },
      ],
    })));

    await render("doc-mrz", () => expect(container?.querySelector(".facts-section")).not.toBeNull());
    expect(container?.querySelector(".facts-section")?.textContent).not.toContain(rawMrz);
    expect(container?.querySelector(".facts-section")?.textContent).toContain("DEMO-PASSPORT");
    expect(container?.querySelectorAll(".facts-list .fact-promote")).toHaveLength(1);
  });

  it("treats numbered MRZ line keys from the model as raw MRZ", async () => {
    const base = await api.document("doc-passport");
    const lines = ["IDUTOD231458907<<<<<<<<<<<<<<<", "7408122F1204159UTO<<<<<<<<<<<6", "ERIKSSON<<ANNA<MARIA<<<<<<<<<<"];
    server.use(http.get("/api/v1/documents/doc-mrz-numbered", () => HttpResponse.json({
      ...base,
      id: "doc-mrz-numbered",
      facts: [
        ...lines.map((value, index) => ({ key: `mrz_line_${index + 1}`, value, provenance: "llm", page: 2 })),
        { key: "archive_reference", value: "DEMO-PASSPORT", provenance: "local", page: 1 },
      ],
    })));

    await render("doc-mrz-numbered", () => expect(container?.querySelector(".facts-section")).not.toBeNull());
    const text = container?.querySelector(".facts-section")?.textContent ?? "";
    for (const line of lines) expect(text).not.toContain(line);
    expect(text).toContain("DEMO-PASSPORT");
  });

  it("keeps an unparsed MRZ line available but never offers to promote it", async () => {
    const base = await api.document("doc-rental");
    const rawMrz = "P<UTOERIKSSON<<ANNA<MARIA<<<<<<<<<<<<<<<<<<<L898902C36UTO7408122F1204159ZE184226B<<<<<10";
    server.use(http.get("/api/v1/documents/doc-mrz-raw", () => HttpResponse.json({
      ...base,
      id: "doc-mrz-raw",
      facts: [{ key: "mrz_line", value: rawMrz, provenance: "local", page: 1 }],
    })));

    await render("doc-mrz-raw", () => expect(container?.querySelector(".facts-section")).not.toBeNull());
    expect(container?.querySelector(".fact-value")?.textContent).toBe(rawMrz);
    expect(container?.querySelector(".facts-list .fact-promote")).toBeNull();
    expect(container?.querySelector('.facts-list [role="combobox"]')).toBeNull();
  });
});

describe("DocumentPage organizations editor", () => {
  it("selects heading based on shared role or document type default", async () => {
    await render("doc-passport", () => {
      expect(container?.querySelector(".organizations-editor > span")?.textContent).toBe("Issued by");
    });

    await render("doc-rental", () => {
      expect(container?.querySelector(".organizations-editor > span")?.textContent).toBe("Parties");
    });
  });

  it("adds an organization and sends the full list with the default role", async () => {
    const patch = vi.spyOn(api, "patchDocument");
    const base = await api.document("doc-rental");
    server.use(http.get("/api/v1/documents/doc-test-add", () => HttpResponse.json({
      ...base,
      id: "doc-test-add",
      type_key: "passport",
      organizations: [],
    })));

    await render("doc-test-add", () => expect(button("Add organization")).toBeTruthy());
    await click(button("Add organization"));

    await eventually(() => expect(byLabel("Organization")).not.toBeNull());
    expect(shown("Organization")).toBe("Not specified");
    expect(shown("Role")).toBe("Issued by");

    await choose("Organization", "North Bank");

    await eventually(() => expect(patch).toHaveBeenCalledWith("doc-test-add", {
      organizations: [{ id: "corr-north", role: "issued_by" }],
    }));
  });

  it("changing a role sends the full list", async () => {
    const patch = vi.spyOn(api, "patchDocument");
    const base = await api.document("doc-rental");
    server.use(http.get("/api/v1/documents/doc-role-test", () => HttpResponse.json({
      ...base,
      id: "doc-role-test",
      organizations: [{ id: "corr-city", name: "City Housing", role: "party", provenance: "manual" }],
    })));

    await render("doc-role-test", () => expect(byLabel("Role")).not.toBeNull());
    expect(shown("Role")).toBe("Contract party");

    await choose("Role", "Employer");

    await eventually(() => expect(patch).toHaveBeenCalledWith("doc-role-test", {
      organizations: [{ id: "corr-city", role: "employer" }],
    }));
  });

  it("removing the last row sends an empty list []", async () => {
    const patch = vi.spyOn(api, "patchDocument");
    const base = await api.document("doc-rental");
    server.use(http.get("/api/v1/documents/doc-remove-test", () => HttpResponse.json({
      ...base,
      id: "doc-remove-test",
      organizations: [{ id: "corr-city", name: "City Housing", role: "party", provenance: "manual" }],
    })));

    await render("doc-remove-test", () => expect(button("Remove organization")).toBeTruthy());

    await click(button("Remove organization"));

    await eventually(() => expect(patch).toHaveBeenCalledWith("doc-remove-test", {
      organizations: [],
    }));
  });

  it("a draft row with no organization sends nothing and disappears when removed", async () => {
    const patch = vi.spyOn(api, "patchDocument");
    const base = await api.document("doc-rental");
    server.use(http.get("/api/v1/documents/doc-draft-test", () => HttpResponse.json({
      ...base,
      id: "doc-draft-test",
      organizations: [],
    })));

    await render("doc-draft-test", () => expect(button("Add organization")).toBeTruthy());
    await click(button("Add organization"));

    await eventually(() => expect(byLabel("Organization")).not.toBeNull());
    await choose("Role", "Employer");

    expect(patch).not.toHaveBeenCalled();

    await click(button("Remove organization"));
    await eventually(() => expect(container?.textContent).toContain("Not specified"));
    expect(patch).not.toHaveBeenCalled();
  });

  it("a failed save shows the error and does not keep the unsaved state", async () => {
    vi.spyOn(api, "patchDocument").mockRejectedValue(new ApiError(500, "network", "Save failed"));
    const base = await api.document("doc-rental");
    server.use(http.get("/api/v1/documents/doc-fail-test", () => HttpResponse.json({
      ...base,
      id: "doc-fail-test",
      organizations: [{ id: "corr-city", name: "City Housing", role: "party", provenance: "manual" }],
    })));

    await render("doc-fail-test", () => expect(byLabel("Role")).not.toBeNull());
    expect(shown("Role")).toBe("Contract party");

    await choose("Role", "Employer");

    await eventually(() => {
      const banner = container?.querySelector(".document-warning");
      expect(banner).not.toBeNull();
      expect(shown("Role")).toBe("Contract party");
    });
  });
});

/** Types into a controlled input the way a keyboard does: React reads the native value setter's event. */
async function type(input: HTMLInputElement, text: string) {
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(input, text);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

async function press(element: Element, key: string) {
  await act(async () => { element.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true })); });
}

function pickerInput(label: string): HTMLInputElement {
  const input = byLabel<HTMLInputElement>(label);
  if (!input) throw new Error(`Input not found: ${label}`);
  return input;
}

function createOption(): HTMLButtonElement | undefined {
  return Array.from(container?.querySelectorAll<HTMLButtonElement>(".metadata-picker-create") ?? [])[0];
}

describe("DocumentPage creating from the pickers", () => {
  // The mock server adds what is created to its shared taxonomy; take it back out.
  const people = taxonomy.people.length;
  const tags = taxonomy.tags.length;
  afterEach(() => {
    taxonomy.people.length = people;
    taxonomy.tags.length = tags;
  });

  it("creates a person that matches nobody and puts them on the document", async () => {
    const createPerson = vi.spyOn(api, "createPerson");
    const patchDocument = vi.spyOn(api, "patchDocument");
    const before = (await api.document("doc-passport")).people.map((person) => person.id);
    await render("doc-passport", () => expect(byLabel("Find or add a person")).not.toBeNull());

    expect(createOption()).toBeUndefined();
    await type(pickerInput("Find or add a person"), "  Ivan   Testov ");
    expect(createOption()?.textContent).toBe("Create “Ivan Testov”");
    await click(createOption()!);

    await eventually(() => {
      expect(createPerson).toHaveBeenCalledWith({ name: "Ivan Testov" });
      expect(patchDocument).toHaveBeenCalledWith("doc-passport", { people: [...before, expect.stringMatching(/^person-/)] });
    });
    // What was typed is cleared once the person is on the document.
    await eventually(() => expect(pickerInput("Find or add a person").value).toBe(""));
  });

  it("creates a tag with the keyboard alone: type, then Enter", async () => {
    const createTag = vi.spyOn(api, "createTag");
    const patchDocument = vi.spyOn(api, "patchDocument");
    const before = (await api.document("doc-passport")).tags.map((tag) => tag.id);
    await render("doc-passport", () => expect(byLabel("Find or add a tag")).not.toBeNull());

    const input = pickerInput("Find or add a tag");
    await type(input, "health");
    await press(input, "Enter");

    await eventually(() => {
      expect(createTag).toHaveBeenCalledWith({ name: "health" });
      expect(patchDocument).toHaveBeenCalledWith("doc-passport", { tags: [...before, expect.stringMatching(/^tag-/)] });
    });
  });

  it("offers nothing to create for a name that exists, however it is cased, and Enter does nothing", async () => {
    const createPerson = vi.spyOn(api, "createPerson");
    await render("doc-passport", () => expect(byLabel("Find or add a person")).not.toBeNull());
    const input = pickerInput("Find or add a person");

    await type(input, "alex EXAMPLE");
    expect(createOption()).toBeUndefined();
    await press(input, "Enter");
    // The list narrows to the match instead.
    const shown = Array.from(container?.querySelectorAll(".metadata-picker")[0]?.querySelectorAll("label span") ?? []).map((node) => node.textContent);
    expect(shown).toEqual(["Alex Example"]);
    expect(createPerson).not.toHaveBeenCalled();
  });

  it("says so, and keeps what was typed, when the person cannot be created", async () => {
    const patchDocument = vi.spyOn(api, "patchDocument");
    server.use(http.post("/api/v1/people", () => HttpResponse.json({ code: "internal", message: "no" }, { status: 500 })));
    await render("doc-passport", () => expect(byLabel("Find or add a person")).not.toBeNull());

    await type(pickerInput("Find or add a person"), "Ivan Testov");
    await click(createOption()!);

    await eventually(() => expect(document.body.textContent).toContain("Could not create it. Try again."));
    expect(patchDocument).not.toHaveBeenCalled();
    expect(pickerInput("Find or add a person").value).toBe("Ivan Testov");
  });

  it("is written in Russian for a Russian interface", async () => {
    await render("doc-passport", () => expect(byLabel("Найти или добавить человека")).not.toBeNull(), "ru");
    await type(pickerInput("Найти или добавить человека"), "Иван Тестов");
    expect(createOption()?.textContent).toBe("Создать «Иван Тестов»");
  });
});

describe("DocumentPage due date", () => {
  const daysFromNow = (days: number) => new Date(Date.now() + days * 86_400_000).toISOString().slice(0, 10);

  async function renderWithDue(due: string | null) {
    const base = await api.document("doc-passport");
    server.use(http.get("/api/v1/documents/doc-due", () => HttpResponse.json({ ...base, id: "doc-due", due_date: due })));
    await render("doc-due", () => expect(container?.querySelector(".metadata-section")).not.toBeNull());
  }

  it("shows nothing for a document with no due date", async () => {
    await renderWithDue(null);
    expect(container?.querySelector(".due-date")).toBeNull();
  });

  it("shows how near a due date is, and marks one that is near or past", async () => {
    await renderWithDue(daysFromNow(30));
    await eventually(() => expect(container?.querySelector(".due-date")).not.toBeNull());
    expect(container?.querySelector(".due-date")?.textContent).toContain("Due");
    expect(container?.querySelector(".due-date")?.textContent).toContain("in 30 days");
    expect(container?.querySelector(".due-date")?.classList.contains("due-date--soon")).toBe(true);
  });

  it("says overdue for a date that has passed and nothing for a far one", async () => {
    await renderWithDue(daysFromNow(-3));
    await eventually(() => expect(container?.querySelector(".due-date")?.textContent).toContain("overdue"));
    await act(async () => root?.unmount());
    container?.remove();

    await renderWithDue(daysFromNow(900));
    await eventually(() => expect(container?.querySelector(".due-date")).not.toBeNull());
    expect(container?.querySelector(".due-date small")).toBeNull();
    expect(container?.querySelector(".due-date")?.classList.contains("due-date--soon")).toBe(false);
  });
});

describe("DocumentPage source and history", () => {
  it("shows how a web-uploaded document entered the archive and lists its event history on demand", async () => {
    await render("doc-rental", () => expect(container?.querySelector(".document-source-line")).not.toBeNull());

    expect(container?.querySelector(".document-source-line")?.textContent).toContain("Added via web interface");
    const history = container?.querySelector<HTMLDetailsElement>(".document-history");
    expect(history).not.toBeNull();
    expect(history?.open).toBe(false);
    expect(history?.textContent).toContain("History");
    expect(history?.textContent).toContain("3 events");

    await click(history!.querySelector("summary")!);
    expect(history?.open).toBe(true);
    const items = Array.from(history?.querySelectorAll(".document-history-list li") ?? []).map((item) => item.textContent);
    expect(items).toHaveLength(3);
    expect(items[0]).toContain("Updated");
    expect(items[0]).toContain("Web");
    expect(items[0]).toContain("Demo user");
    expect(items[1]).toContain("Processed");
    expect(items[1]).toContain("System");
    expect(items[2]).toContain("Uploaded");
  });

  it("shows email ingestion source and localizes history in Russian", async () => {
    await render("doc-receipt", () => expect(container?.querySelector(".document-source-line")).not.toBeNull(), "ru");

    expect(container?.querySelector(".document-source-line")?.textContent).toContain("Добавлен по почте");
    const history = container?.querySelector<HTMLDetailsElement>(".document-history");
    expect(history?.textContent).toContain("История");
    expect(history?.textContent).toContain("3 события");
    await click(history!.querySelector("summary")!);
    expect(history?.textContent).toContain("Загружен");
    expect(history?.textContent).toContain("Почта");
  });
});
