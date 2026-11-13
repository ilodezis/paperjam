import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { I18nextProvider } from "react-i18next";
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { setupServer } from "msw/node";

import { api } from "../api";
import { createI18n } from "../i18n";
import { handlers } from "../mocks/handlers";
import { hrefOf } from "../shell/navigation";
import { ScreenHarness } from "../shell/testing";
import { TaxonomyPage } from "./TaxonomyPage";

const server = setupServer(...handlers);
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root | null = null;
let container: HTMLDivElement | null = null;
let queryClient: QueryClient | null = null;

beforeAll(() => server.listen());
afterEach(async () => {
  server.resetHandlers();
  vi.restoreAllMocks();
  if (root) await act(async () => root?.unmount());
  container?.remove();
  root = null;
  container = null;
  queryClient?.clear();
  queryClient = null;
});
afterAll(() => server.close());

async function renderPage(language = "en", expectEditor = true, browserHistory = false) {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  const i18n = createI18n(language);
  await act(async () => {
    root?.render(
      <QueryClientProvider client={queryClient!}>
        <I18nextProvider i18n={i18n}>
          <ScreenHarness browserHistory={browserHistory}><TaxonomyPage /></ScreenHarness>
        </I18nextProvider>
      </QueryClientProvider>
    );
  });
  await eventually(() => expect(container?.querySelector(".taxonomy-list")).not.toBeNull());
  if (expectEditor) await eventually(() => expect(container?.querySelector(".taxonomy-properties")).not.toBeNull());
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
  await act(async () => {
    element.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  });
}

/** Opens a custom select and clicks one of its options, the way a person does. */
async function choose(label: string, optionText: string) {
  await click(byLabel(label)!);
  const option = Array.from(container?.querySelectorAll('[role="option"]') ?? []).find((node) => node.textContent === optionText);
  if (!option) throw new Error(`Option not found: ${optionText}`);
  await click(option);
}

async function change(element: HTMLElement, value: string) {
  const prototype = element instanceof HTMLSelectElement ? HTMLSelectElement.prototype : element instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  Object.getOwnPropertyDescriptor(prototype, "value")?.set?.call(element, value);
  await act(async () => {
    element.dispatchEvent(new Event(element instanceof HTMLSelectElement ? "change" : "input", { bubbles: true }));
  });
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

describe("TaxonomyPage", () => {
  it("uses directory names and only the three entity tabs", async () => {
    await renderPage();
    expect(container?.querySelector(".taxonomy-header h1")?.textContent).toBe("People & organizations");
    expect(Array.from(container?.querySelectorAll('[role="tab"]') ?? []).map((tab) => tab.textContent)).toEqual(["People2", "Organizations3", "Tags3"]);
  });

  it("labels a person's name as Имя and an organization's as Название", async () => {
    await renderPage("ru");
    expect(container?.querySelector(".taxonomy-properties .field-label span")?.textContent).toBe("Имя");

    await click(button("Добавить человека"));
    expect(container?.querySelector(".taxonomy-create .field-label span")?.textContent).toBe("Имя");

    await click(byLabel("Организации")!);
    await click(button("Добавить организацию"));
    expect(container?.querySelector(".taxonomy-create .field-label span")?.textContent).toBe("Название");
  });

  it("creates a person and opens the new record", async () => {
    await renderPage();
    await click(button("Add person"));
    await change(byLabel("New person name")!, "Taylor Synthetic");
    await click(button("Create person"));

    await eventually(() => expect((byLabel<HTMLInputElement>("Name for Taylor Synthetic")?.value)).toBe("Taylor Synthetic"));
  });

  it("adds an alias to the selected person", async () => {
    await renderPage();
    await change(byLabel("Another spelling for Alex Example")!, "Al");
    await click(byLabel("Add another spelling for Alex Example")!);

    await eventually(() => expect(container?.textContent).toContain("Al"));
    const alex = (await api.taxonomy()).people.find((person) => person.id === "person-alex");
    expect(alex?.aliases).toEqual(["A. Example", "Al"]);
  });

  it("merges a duplicate only after confirmation", async () => {
    await renderPage();
    vi.spyOn(window, "confirm").mockReturnValue(true);
    await choose("Duplicate to merge into Alex Example", "Morgan Sample");
    await click(byLabel("Merge into Alex Example")!);

    await eventually(() => expect(byLabel("Name for Morgan Sample")).toBeNull());
    const alex = (await api.taxonomy()).people.find((person) => person.id === "person-alex");
    expect(alex?.aliases).toEqual(["A. Example", "Al", "Morgan Sample"]);
    await eventually(() => expect(byLabel<HTMLButtonElement>("Merge into Alex Example")?.disabled).toBe(true));
    const trigger = byLabel("Duplicate to merge into Alex Example");
    if (trigger) {
      await click(trigger);
      const labels = Array.from(container?.querySelectorAll('[role="option"]') ?? []).map((node) => node.textContent);
      expect(labels).not.toContain("Morgan Sample");
    }
  });

  it("renames a tag", async () => {
    await renderPage();
    await click(button("Tags"));
    await change(byLabel("Name for warranty")!, "guarantee");
    await click(byLabel("Save warranty")!);

    await eventually(() => expect((byLabel<HTMLInputElement>("Name for guarantee")?.value)).toBe("guarantee"));
  });

  it("deletes a tag after confirmation", async () => {
    await renderPage();
    await click(button("Tags"));
    await click(button("renew"));
    vi.spyOn(window, "confirm").mockReturnValue(true);
    await click(byLabel("Delete renew")!);

    await eventually(() => expect(byLabel("Name for renew")).toBeNull());
  });

  it("deletes a person after confirmation", async () => {
    await renderPage();
    await click(button("Add person"));
    await change(byLabel("New person name")!, "Temporary Person");
    await click(button("Create person"));
    await eventually(() => expect(byLabel("Name for Temporary Person")).not.toBeNull());

    vi.spyOn(window, "confirm").mockReturnValue(true);
    await click(byLabel("Delete Temporary Person")!);

    await eventually(() => expect(byLabel("Name for Temporary Person")).toBeNull());
  });

  it("keeps document schemas out of the everyday directories", async () => {
    await renderPage();

    expect(Array.from(container?.querySelectorAll('[role="tab"]') ?? []).map((element) => element.getAttribute("aria-label"))).toEqual(["People", "Organizations", "Tags"]);
    expect(byLabel("Document types")).toBeNull();
    expect(container?.querySelector(".type-editor")).toBeNull();
  });

  it("keeps one create form inside the detail panel and closes a clean form on cancel", async () => {
    await renderPage();
    await click(button("Add person"));

    expect(container?.querySelectorAll(".taxonomy-create").length).toBe(1);
    expect(container?.querySelector(".taxonomy-detail .taxonomy-create")).not.toBeNull();

    await click(button("Cancel"));
    expect(container?.querySelector(".taxonomy-create")).toBeNull();
    expect(container?.querySelector(".taxonomy-detail .taxonomy-properties")).not.toBeNull();
  });

  it("links a person's documents to the archive filtered by that person", async () => {
    await renderPage();
    const link = Array.from(container?.querySelectorAll<HTMLAnchorElement>("a") ?? []).find((anchor) => anchor.textContent?.includes("Show documents"));
    expect(link).toBeDefined();

    const url = new URL(link!.getAttribute("href") ?? "", "http://x");
    expect(url.pathname).toBe(hrefOf("archive"));
    expect(url.searchParams.get("q")).toBe('person:"Alex Example"');
  });

  it("places documents beside the entity identity instead of the save action", async () => {
    await renderPage();
    const alex = (await api.taxonomy()).people.find((person) => person.id === "person-alex");
    const identity = container?.querySelector(".taxonomy-identity");
    expect(identity?.querySelector("h2")?.textContent).toBe("Alex Example");
    expect(identity?.textContent).toContain(`${alex?.document_count} documents`);
    expect(identity?.querySelector("a.taxonomy-documents-link")).not.toBeNull();
    expect(container?.querySelector(".taxonomy-actions a.taxonomy-documents-link")).toBeNull();
  });

  it("shows aliases and merging as visible sections and deletion in a danger zone, with no disclosures", async () => {
    await renderPage();
    const editor = container?.querySelector(".taxonomy-detail");
    expect(editor).not.toBeNull();
    expect(editor?.querySelectorAll("details")).toHaveLength(0);

    expect(editor?.querySelector("section.taxonomy-secondary h3.taxonomy-secondary-title")?.textContent).toBe("Also known as");
    expect(editor?.querySelector("section.taxonomy-merge h3.taxonomy-secondary-title")?.textContent).toBe("Merge with another record");
    expect(editor?.querySelector("div.taxonomy-danger-zone button.taxonomy-delete")).not.toBeNull();
    expect(byLabel("Delete Alex Example")).not.toBeNull();
  });

  it("switches between list and detail on mobile without losing the selected record", async () => {
    await renderPage();
    expect(container?.querySelector(".taxonomy-workspace--detail-open")).toBeNull();

    await click(button("Taylor Synthetic"));
    expect(container?.querySelector(".taxonomy-workspace--detail-open")).not.toBeNull();
    expect(byLabel("Name for Taylor Synthetic")).not.toBeNull();

    await click(button("Back to list"));
    expect(container?.querySelector(".taxonomy-workspace--detail-open")).toBeNull();
    await click(button("Taylor Synthetic"));
    expect(byLabel<HTMLInputElement>("Name for Taylor Synthetic")?.value).toBe("Taylor Synthetic");
  });

  it("keeps a dirty record open when switching records is cancelled", async () => {
    await renderPage();
    await change(byLabel("Name for Alex Example")!, "Unsaved Alex");
    vi.spyOn(window, "confirm").mockReturnValue(false);

    await click(button("Taylor Synthetic"));

    expect(window.confirm).toHaveBeenCalledTimes(1);
    expect(byLabel<HTMLInputElement>("Name for Alex Example")?.value).toBe("Unsaved Alex");
    expect(byLabel("Name for Taylor Synthetic")).toBeNull();
    expect(container?.textContent).toContain("Unsaved changes");
  });

  it("switches records after explicitly discarding unsaved changes", async () => {
    await renderPage();
    await change(byLabel("Name for Alex Example")!, "Unsaved Alex");
    vi.spyOn(window, "confirm").mockReturnValue(true);

    await click(button("Taylor Synthetic"));

    expect(window.confirm).toHaveBeenCalledTimes(1);
    await eventually(() => expect(byLabel<HTMLInputElement>("Name for Taylor Synthetic")?.value).toBe("Taylor Synthetic"));
  });

  it("keeps a dirty create form open when cancellation is rejected", async () => {
    await renderPage();
    await click(button("Add person"));
    await change(byLabel("New person name")!, "Unsaved person");
    vi.spyOn(window, "confirm").mockReturnValue(false);

    await click(button("Cancel"));

    expect(window.confirm).toHaveBeenCalledTimes(1);
    expect(byLabel<HTMLInputElement>("New person name")?.value).toBe("Unsaved person");
  });

  it("warns before browser unload discards changes", async () => {
    await renderPage("en", true, true);
    await change(byLabel("Name for Alex Example")!, "Unsaved Alex");

    const unload = new Event("beforeunload", { cancelable: true });
    window.dispatchEvent(unload);
    expect(unload.defaultPrevented).toBe(true);
  });

  it("asks before a document link discards changes", async () => {
    await renderPage("en", true, true);
    await change(byLabel("Name for Alex Example")!, "Unsaved Alex");
    vi.spyOn(window, "confirm").mockReturnValue(false);

    const link = container?.querySelector<HTMLAnchorElement>("a.taxonomy-documents-link");
    await click(link!);
    await eventually(() => expect(window.confirm).toHaveBeenCalledTimes(1));
  });

  it("shows a single add action and no empty detail pane for an empty directory", async () => {
    const { http, HttpResponse } = await import("msw");
    const { taxonomy } = await import("../mocks/data");
    server.use(http.get("/api/v1/taxonomy", () => HttpResponse.json({ ...taxonomy, people: [] })));
    await renderPage("en", false);

    expect(container?.querySelector(".taxonomy-detail")).toBeNull();
    expect(container?.querySelector(".taxonomy-empty button")).toBeNull();
    expect(Array.from(container?.querySelectorAll("button") ?? []).filter((element) => element.textContent === "Add person")).toHaveLength(1);
  });
});
