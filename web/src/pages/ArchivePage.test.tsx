import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { I18nextProvider } from "react-i18next";
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { http, HttpResponse } from "msw";
import { setupServer } from "msw/node";

import { api } from "../api";
import { ToastHost } from "../components/controls/ToastHost";
import { clearToasts } from "../components/controls/toast";
import { createI18n } from "../i18n";
import { expiringDocuments, hits, taxonomy } from "../mocks/data";
import { handlers } from "../mocks/handlers";
import { ScreenHarness } from "../shell/testing";
import { ArchivePage } from "./ArchivePage";

const server = setupServer(...handlers);
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root | null = null;
let container: HTMLDivElement | null = null;

beforeAll(() => {
  server.listen();
  // jsdom has no Web Animations: the filters sheet animates in and out.
  Element.prototype.animate = (() => ({ finished: Promise.resolve(), cancel: () => undefined })) as unknown as Element["animate"];
});
afterEach(async () => {
  server.resetHandlers();
  vi.restoreAllMocks();
  clearToasts();
  vi.unstubAllGlobals();
  if (root) await act(async () => root?.unmount());
  container?.remove();
  root = null;
  container = null;
});
afterAll(() => server.close());

async function render(search = "", ready: () => void = () => expect(button("Select")).toBeTruthy()) {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  await act(async () => {
    root?.render(
      <QueryClientProvider client={queryClient}>
        <I18nextProvider i18n={createI18n("en")}>
          <ScreenHarness search={search}><ArchivePage /></ScreenHarness>
          <ToastHost />
        </I18nextProvider>
      </QueryClientProvider>
    );
  });
  await eventually(ready);
}

function button(name: string): HTMLButtonElement {
  const match = Array.from(container?.querySelectorAll("button") ?? []).find((element) => element.textContent?.trim() === name || element.getAttribute("aria-label") === name);
  if (!match) throw new Error(`Button not found: ${name}`);
  return match;
}

function checkboxes(): HTMLInputElement[] {
  return Array.from(container?.querySelectorAll<HTMLInputElement>(".result-select input") ?? []);
}

async function click(element: Element) {
  await act(async () => { element.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
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

describe("ArchivePage selection", () => {
  it("shows checkboxes only in selection mode and reprocesses the chosen documents", async () => {
    const reprocess = vi.spyOn(api, "reprocess");
    await render();
    expect(checkboxes()).toHaveLength(0);

    await click(button("Select"));
    await eventually(() => expect(checkboxes().length).toBeGreaterThan(1));
    await click(checkboxes()[0]);
    await click(checkboxes()[1]);
    expect(container?.textContent).toContain("2 documents selected");

    await click(button("Reprocess"));
    await eventually(() => expect(container?.textContent).toContain("2 documents queued for reprocessing"));
    expect(reprocess).toHaveBeenCalledTimes(2);
    expect(reprocess).toHaveBeenCalledWith(expect.any(String), true);
  });

  it("moves the selection to trash and Undo restores every document", async () => {
    const restore = vi.spyOn(api, "restoreDocument");
    const trash = vi.spyOn(api, "trashDocument");
    await render();
    await click(button("Select"));
    await eventually(() => expect(checkboxes().length).toBeGreaterThan(1));
    await click(checkboxes()[0]);
    await click(checkboxes()[1]);

    await click(button("Move to trash"));
    await eventually(() => expect(container?.textContent).toContain("2 documents moved to trash"));
    const trashed = trash.mock.calls.map(([id]) => id);
    await click(button("Undo"));
    await eventually(() => expect(restore.mock.calls.map(([id]) => id).sort()).toEqual([...trashed].sort()));
  });

  it("leaves selection mode with the close button", async () => {
    await render();
    await click(button("Select"));
    await eventually(() => expect(checkboxes().length).toBeGreaterThan(0));
    await click(button("Stop selecting"));
    await eventually(() => expect(checkboxes()).toHaveLength(0));
  });

  it("leaves selection mode and hides the toolbar once trashing empties the archive", async () => {
    await render();
    await click(button("Select"));
    await eventually(() => expect(checkboxes().length).toBeGreaterThan(0));
    const selectAll = Array.from(container?.querySelectorAll("button") ?? []).find((element) => element.textContent?.startsWith("Select all"));
    await click(selectAll!);
    await click(button("Move to trash"));

    await eventually(() => expect(container?.querySelector(".search-form")).toBeNull());
    expect(container?.querySelector(".results-meta")).toBeNull();
    expect(checkboxes()).toHaveLength(0);
    expect(container?.textContent).toContain("Your archive is ready");
    // The mock archive is shared by the whole file: put the documents back for the tests after this one.
    for (const hit of hits) await api.restoreDocument(hit.id);
  });
});

describe("ArchivePage attention", () => {
  it("marks a result that needs attention on its card", async () => {
    await render();

    const card = Array.from(container?.querySelectorAll(".result-row") ?? []).find((row) => row.querySelector(".result-attention"));
    expect(card?.querySelector(".result-title")?.textContent).toBe("Clinic visit summary");
    expect(card?.querySelector(".result-attention")?.textContent).toContain("Needs attention");
  });
});

describe("ArchivePage thumbnails", () => {
  it("asks for no thumbnail of a document without page images", async () => {
    server.use(http.post("/api/v1/search", () => HttpResponse.json({
      hits: hits.map((hit) => (hit.id === "doc-medical" ? { ...hit, has_pages: false } : hit)),
      total: hits.length,
      next_cursor: null,
      partial: false,
    })));

    await render();
    await eventually(() => expect(container?.querySelectorAll(".result-row")).toHaveLength(hits.length));

    const rows = Array.from(container?.querySelectorAll(".result-row") ?? []);
    const thumbnailOf = (title: string) => rows.find((row) => row.querySelector(".result-title")?.textContent === title)?.querySelector(".result-thumbnail img");
    expect(thumbnailOf("Clinic visit summary")).toBeNull();
    expect(thumbnailOf("Passport — Alex Example")?.getAttribute("src")).toBe("/api/v1/documents/doc-passport/thumbnail.webp");
  });
});

describe("ArchivePage quick filters", () => {
  it("refreshes a cached taxonomy once when results bring a type it has not counted", async () => {
    let calls = 0;
    server.use(http.get("/api/v1/taxonomy", () => {
      calls += 1;
      // The first copy predates the passport upload; the second one counts it.
      const types = taxonomy.types.map((type) => (type.key === "passport" ? { ...type, document_count: calls === 1 ? 0 : 1 } : type));
      return HttpResponse.json({ ...taxonomy, types });
    }));

    await render();
    await eventually(() => expect(Array.from(container?.querySelectorAll(".quick-filters .filter-chip") ?? []).some((chip) => chip.textContent === "Passport")).toBe(true));
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(calls).toBe(2);
  });
});

describe("ArchivePage empty and partial results", () => {
  const nothing = http.post("/api/v1/search", () => HttpResponse.json({ hits: [], total: 0, next_cursor: null, partial: false }));

  it("offers no filter reset when the query is words only", async () => {
    server.use(nothing);
    await render("?q=passport%20mom", () => expect(container?.querySelector(".empty-results")).toBeTruthy());
    expect(container?.querySelector(".empty-results")?.textContent).not.toContain("Clear all filters");
  });

  it("offers the reset when a filter is in the query", async () => {
    server.use(nothing);
    await render("?q=passport%20type%3Apassport", () => expect(container?.querySelector(".empty-results")).toBeTruthy());
    await eventually(() => expect(container?.querySelector(".empty-results")?.textContent).toContain("Clear all filters"));
  });

  it("says so when the results match only some of the words", async () => {
    server.use(http.post("/api/v1/search", () => HttpResponse.json({ hits: hits.slice(0, 2), total: 2, next_cursor: null, partial: true })));
    await render("?q=passport%20mom");
    await eventually(() => expect(container?.querySelector(".partial-note")?.textContent).toContain("Nothing matched the whole query"));
    expect(container?.querySelectorAll(".results-list li").length).toBe(2);
  });

  it("shows no note on a full match", async () => {
    await render("?q=passport");
    await eventually(() => expect(container?.querySelector(".results-list")).toBeTruthy());
    expect(container?.querySelector(".partial-note")).toBeNull();
  });
});

describe("ArchivePage load more", () => {
  it("shows an inline error with Retry button on load-more failure, and retrying succeeds without duplicates", async () => {
    let callCount = 0;
    server.use(
      http.post("/api/v1/search", async ({ request }) => {
        const body = (await request.json()) as { cursor?: string };
        if (!body.cursor) {
          return HttpResponse.json({
            hits: [
              { id: "hit-1", title: "Document 1", snippet: "First", people: [], key_fields: {} },
            ],
            total: 3,
            next_cursor: "cursor-2",
          });
        }
        callCount += 1;
        if (callCount === 1) {
          return new HttpResponse(null, { status: 500 });
        }
        return HttpResponse.json({
          hits: [
            { id: "hit-1", title: "Document 1", snippet: "First", people: [], key_fields: {} },
            { id: "hit-2", title: "Document 2", snippet: "Second", people: [], key_fields: {} },
          ],
          total: 3,
          next_cursor: null,
        });
      })
    );

    await render();
    expect(button("Show more documents")).toBeTruthy();
    expect(container?.querySelectorAll(".result-row")).toHaveLength(1);

    await click(button("Show more documents"));
    await eventually(() => expect(container?.querySelector(".load-more-error")).not.toBeNull());
    expect(container?.textContent).toContain("Could not load more documents.");
    expect(button("Try again")).toBeTruthy();

    await click(button("Try again"));
    await eventually(() => expect(container?.querySelector(".load-more-error")).toBeNull());
    expect(container?.querySelectorAll(".result-row")).toHaveLength(2);
    expect(container?.textContent).toContain("Document 1");
    expect(container?.textContent).toContain("Document 2");

    const titles = Array.from(container?.querySelectorAll(".result-title") ?? []).map((el) => el.textContent);
    expect(titles).toEqual(["Document 1", "Document 2"]);
    expect(container?.querySelector(".load-more")).toBeNull();
  });

  it("continues to show the load-more button when a subsequent page provides another cursor", async () => {
    server.use(
      http.post("/api/v1/search", async ({ request }) => {
        const body = (await request.json()) as { cursor?: string };
        if (!body.cursor) {
          return HttpResponse.json({
            hits: [{ id: "hit-1", title: "Page 1 Doc", snippet: "First", people: [], key_fields: {} }],
            total: 3,
            next_cursor: "cursor-2",
          });
        }
        if (body.cursor === "cursor-2") {
          return HttpResponse.json({
            hits: [{ id: "hit-2", title: "Page 2 Doc", snippet: "Second", people: [], key_fields: {} }],
            total: 3,
            next_cursor: "cursor-3",
          });
        }
        return HttpResponse.json({
          hits: [{ id: "hit-3", title: "Page 3 Doc", snippet: "Third", people: [], key_fields: {} }],
          total: 3,
          next_cursor: null,
        });
      })
    );

    await render();
    expect(container?.querySelectorAll(".result-row")).toHaveLength(1);
    expect(button("Show more documents")).toBeTruthy();

    await click(button("Show more documents"));
    await eventually(() => expect(container?.querySelectorAll(".result-row")).toHaveLength(2));
    expect(button("Show more documents")).toBeTruthy();

    await click(button("Show more documents"));
    await eventually(() => expect(container?.querySelectorAll(".result-row")).toHaveLength(3));
    expect(container?.querySelector(".load-more")).toBeNull();
  });
});

describe("ArchivePage toolbar", () => {
  it("offers All documents only while a query is active", async () => {
    await render();
    expect(() => button("All documents")).toThrow();
    expect(container?.textContent).not.toContain("All documents");
  });

  it("offers All documents as the way back from an active query", async () => {
    await render("q=type:passport");
    expect(button("All documents")).toBeTruthy();
  });

  it("keeps rarely used options in a Filters sheet that reflects the active filters", async () => {
    await render("q=type:passport");
    await click(button("Filters, 1 active"));
    const dialog = await vi.waitFor(() => {
      const found = document.querySelector<HTMLElement>("[role=dialog]");
      if (!found) throw new Error("no dialog");
      return found;
    });
    const chips = Array.from(dialog.querySelectorAll<HTMLButtonElement>("button[aria-pressed]"));
    const pressed = chips.filter((chip) => chip.getAttribute("aria-pressed") === "true").map((chip) => chip.textContent);
    expect(pressed).toContain("Passport");
    expect(pressed).toContain("Relevance");
    expect(dialog.textContent).toContain("Search syntax");
  });

  it("drops a clicked syntax example into the search field and closes the sheet", async () => {
    await render("q=rent");
    await click(button("Filters"));
    const dialog = await vi.waitFor(() => {
      const found = document.querySelector<HTMLElement>("[role=dialog]");
      if (!found) throw new Error("no dialog");
      return found;
    });
    const example = Array.from(dialog.querySelectorAll<HTMLButtonElement>(".syntax-example")).find((item) => item.textContent?.includes("has:attention"));
    expect(example).toBeTruthy();
    await click(example!);
    await vi.waitFor(() => {
      const input = container?.querySelector<HTMLInputElement>(".search-form input");
      expect(input?.value).toBe("rent has:attention");
      expect(document.activeElement).toBe(input);
    });
    expect(document.querySelector("[role=dialog]")).toBeNull();
  });

  it("shows expiring and due date examples in the syntax help and inserts them on click", async () => {
    await render("q=passport");
    await click(button("Filters"));
    const dialog = await vi.waitFor(() => {
      const found = document.querySelector<HTMLElement>("[role=dialog]");
      if (!found) throw new Error("no dialog");
      return found;
    });
    const examples = Array.from(dialog.querySelectorAll<HTMLButtonElement>(".syntax-example"));
    expect(examples.some((item) => item.textContent?.includes("has:expiring"))).toBe(true);
    const dueExample = examples.find((item) => item.textContent?.includes("due:<2027-06"));
    expect(dueExample).toBeTruthy();

    await click(dueExample!);
    await vi.waitFor(() => {
      const input = container?.querySelector<HTMLInputElement>(".search-form input");
      expect(input?.value).toBe("passport due:<2027-06");
      expect(document.activeElement).toBe(input);
    });
    expect(document.querySelector("[role=dialog]")).toBeNull();
  });

  it("hands focus to the search input after picking an example even while the sheet exit animation is still running", async () => {
    const prevAnimate = Element.prototype.animate;
    let calls = 0;
    Element.prototype.animate = (() => {
      calls += 1;
      // Hold closing animations (after the first two opening animations) so the dialog stays mounted briefly.
      return { finished: calls > 2 ? new Promise<void>(() => undefined) : Promise.resolve(), cancel: () => undefined };
    }) as unknown as Element["animate"];
    try {
      await render("q=rent");
      const filtersBtn = button("Filters");
      filtersBtn.focus();
      await click(filtersBtn);
      const dialog = await vi.waitFor(() => {
        const found = document.querySelector<HTMLElement>("[role=dialog]");
        if (!found) throw new Error("no dialog");
        return found;
      });
      const example = Array.from(dialog.querySelectorAll<HTMLButtonElement>(".syntax-example")).find((item) => item.textContent?.includes("has:attention"));
      await click(example!);

      const input = container?.querySelector<HTMLInputElement>(".search-form input");
      expect(input?.value).toBe("rent has:attention");
      expect(input?.closest("[inert]")).toBeNull();
      expect(document.activeElement).toBe(input);
    } finally {
      Element.prototype.animate = prevAnimate;
    }
  });
});

describe("ArchivePage selection state", () => {
  it("starts compact: a hint and no actions until something is selected", async () => {
    await render();
    await click(button("Select"));
    await eventually(() => expect(checkboxes().length).toBeGreaterThan(1));
    expect(container?.textContent).toContain("Select documents to act on them");
    expect(() => button("Reprocess")).toThrow();
    expect(() => button("Move to trash")).toThrow();

    await click(checkboxes()[0]);
    expect(container?.textContent).toContain("1 document selected");
    expect(button("Reprocess").disabled).toBe(false);
    expect(button("Move to trash").disabled).toBe(false);

    await click(checkboxes()[0]);
    expect(() => button("Reprocess")).toThrow();
  });
});

describe("ArchivePage result rows", () => {
  it("calms an uppercase snippet and folds extra people into a count", async () => {
    server.use(
      http.post("/api/v1/search", () => HttpResponse.json({
        hits: [{
          id: "hit-1",
          title: "A very long title that keeps going and going to check that nothing overflows the row",
          snippet: "THE AGREEMENT <mark>BEGINS</mark> ON 15 SEPTEMBER AND RENEWS AUTOMATICALLY",
          organizations: [],
          people: ["Ann Testova", "Bob Testov", "Cy Testov", "Di Testova"],
          key_fields: {},
        }],
        total: 1,
        next_cursor: null,
      })),
    );
    await render();
    expect(container?.querySelector(".result-snippet")?.textContent).toBe("The agreement begins on 15 september and renews automatically");
    expect(container?.querySelector(".text-chip-more")?.textContent).toBe("+2");
    expect(container?.querySelectorAll(".result-taxonomy .text-chip")).toHaveLength(3);
  });

  it("shows two chips and a +N indicator for three organizations", async () => {
    server.use(
      http.post("/api/v1/search", () => HttpResponse.json({
        hits: [{
          id: "hit-orgs",
          title: "Multi-party contract",
          snippet: "Synthetic contract with three organizations.",
          organizations: ["North Bank", "City Housing", "Neighbourhood Market"],
          people: [],
          key_fields: {},
        }],
        total: 1,
        next_cursor: null,
      })),
    );
    await render();
    const chips = Array.from(container?.querySelectorAll(".result-taxonomy .text-chip--plain") ?? []).map((el) => el.textContent?.trim());
    expect(chips).toEqual(["North Bank", "City Housing"]);
    const more = container?.querySelector(".result-taxonomy .text-chip-more");
    expect(more?.textContent).toBe("+1");
    expect(more?.getAttribute("title")).toBe("Neighbourhood Market");
  });
});

function inDays(days: number): string {
  return new Date(Date.now() + days * 86_400_000).toISOString().slice(0, 10);
}

describe("ArchivePage expiring documents", () => {
  afterEach(() => expiringDocuments.clear());

  it("offers no Expiring soon chip while nothing is due soon", async () => {
    await render();
    const labels = Array.from(container?.querySelectorAll(".quick-filters .filter-chip") ?? []).map((chip) => chip.textContent?.trim());
    expect(labels).not.toContain("Expiring soon");
  });

  it("offers the chip, off, when something is due soon", async () => {
    expiringDocuments.add("doc-passport");
    await render("", () => expect(button("Expiring soon")).toBeTruthy());
    expect(button("Expiring soon").getAttribute("aria-pressed")).toBe("false");
    expect(container?.querySelectorAll(".result-row").length).toBeGreaterThan(1);
  });

  it("shows the chip on, with only the expiring documents, while the filter is in the query", async () => {
    expiringDocuments.add("doc-passport");
    await render("q=has%3Aexpiring", () => expect(button("Expiring soon").getAttribute("aria-pressed")).toBe("true"));
    await eventually(() => expect(container?.querySelectorAll(".result-row")).toHaveLength(1));
    expect(container?.querySelector(".result-title")?.textContent).toContain("Passport");
    expect(button("Filters, 1 active")).toBeTruthy();
  });

  it("keeps the chip for a filter that is on even when nothing is due soon, to switch it off", async () => {
    await render("q=has%3Aexpiring", () => expect(button("Expiring soon").getAttribute("aria-pressed")).toBe("true"));
  });

  it("shows the due date on the card, marked when it is near or past", async () => {
    server.use(
      http.post("/api/v1/search", () => HttpResponse.json({
        hits: ["soon", "far", "printed"].map((id, index) => ({
          id: `hit-${id}`,
          title: `Card ${id}`,
          snippet: "",
          organizations: [],
          people: [],
          type_key: "id_card",
          // The third card already prints this very date among its key fields.
          due_date: [inDays(30), inDays(900), inDays(31)][index],
          key_fields: id === "printed" ? { expiry_date: { kind: "date", value: inDays(31) } } : {},
        })),
        total: 3,
        next_cursor: null,
      })),
    );
    await render();
    const rows = Array.from(container?.querySelectorAll(".result-row") ?? []);
    const due = rows.map((row) => row.querySelector(".result-due"));
    expect(due[0]?.classList.contains("result-due--soon")).toBe(true);
    expect(due[0]?.querySelector("dt")?.textContent).toBe("Due");
    expect(due[1]?.classList.contains("result-due--soon")).toBe(false);
    expect(due[1]).not.toBeNull();
    // Not repeated when a key field shows the same date.
    expect(due[2]).toBeNull();
  });
});

/** A stand-in for IntersectionObserver that reports whatever the test says is on screen. */
class FakeIntersectionObserver {
  static instances: FakeIntersectionObserver[] = [];
  static observed = new Set<Element>();
  constructor(readonly callback: IntersectionObserverCallback) {
    FakeIntersectionObserver.instances.push(this);
  }
  observe(element: Element) { FakeIntersectionObserver.observed.add(element); }
  unobserve() {}
  disconnect() {}
  takeRecords() { return []; }
  /** Report one result card as visible. */
  static show(id: string) {
    const target = Array.from(FakeIntersectionObserver.observed).find((element) => element.getAttribute("data-prefetch-id") === id);
    const observer = FakeIntersectionObserver.instances.at(-1);
    if (!target || !observer) throw new Error(`Nothing observes ${id}`);
    observer.callback([{ isIntersecting: true, target } as unknown as IntersectionObserverEntry], observer as unknown as IntersectionObserver);
  }
}

function stubDevice(touch: boolean) {
  FakeIntersectionObserver.instances = [];
  FakeIntersectionObserver.observed = new Set();
  vi.stubGlobal("IntersectionObserver", FakeIntersectionObserver);
  vi.stubGlobal("matchMedia", (query: string) => ({ matches: touch && query.includes("hover: none"), media: query, addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {} }));
}

/** Pathnames of every request the mock server sees from now on. */
function recordRequests(): string[] {
  const paths: string[] = [];
  server.events.on("request:start", ({ request }) => { paths.push(new URL(request.url).pathname); });
  return paths;
}

const idle = () => act(async () => { await new Promise((resolve) => setTimeout(resolve, 300)); });

describe("ArchivePage prefetching", () => {
  afterEach(() => server.events.removeAllListeners());

  it("warms the JSON of a card that scrolls into view on a touch device, once and without page images", async () => {
    stubDevice(true);
    const paths = recordRequests();
    await render();
    await eventually(() => expect(container?.querySelectorAll(".result-row")).toHaveLength(hits.length));

    await act(async () => FakeIntersectionObserver.show("doc-passport"));
    await idle();
    await act(async () => FakeIntersectionObserver.show("doc-passport"));
    await idle();

    expect(paths.filter((path) => path === "/api/v1/documents/doc-passport")).toHaveLength(1);
    expect(paths.filter((path) => /\/pages\/|thumbnail/.test(path))).toEqual([]);
    expect(paths.filter((path) => path.startsWith("/api/v1/documents/") && path !== "/api/v1/documents/doc-passport")).toEqual([]);
  });

  it("warms nothing on a device with hover", async () => {
    stubDevice(false);
    const paths = recordRequests();
    await render();
    await eventually(() => expect(container?.querySelectorAll(".result-row")).toHaveLength(hits.length));
    await idle();

    expect(FakeIntersectionObserver.instances).toHaveLength(0);
    expect(paths.filter((path) => path.startsWith("/api/v1/documents/"))).toEqual([]);
  });
});

describe("ArchivePage re-renders", () => {
  it("leaves the result rows alone when only the page around them changes", async () => {
    // A row's render asks for its thumbnail address, so the calls count the row renders.
    const thumbnail = vi.spyOn(api, "thumbnailUrl");
    await render();
    await eventually(() => expect(container?.querySelectorAll(".result-row")).toHaveLength(hits.length));
    thumbnail.mockClear();

    // Opening the filters sheet re-renders the page but no row.
    await click(button("Filters"));
    await eventually(() => expect(document.querySelector(".filters-sheet")).not.toBeNull());
    expect(thumbnail).not.toHaveBeenCalled();

    // Hovering a row re-renders only the row that lost and the row that gained the highlight.
    const rows = Array.from(container?.querySelectorAll(".result-row") ?? []);
    // No row is lit before the first hover, so the first hover redraws one row.
    await act(async () => { rows[1].dispatchEvent(new PointerEvent("pointerover", { bubbles: true, pointerType: "mouse" })); });
    await eventually(() => expect(rows[1].classList.contains("result-row--active")).toBe(true));
    expect(thumbnail).toHaveBeenCalledTimes(1);
    thumbnail.mockClear();
    await act(async () => { rows[2].dispatchEvent(new PointerEvent("pointerover", { bubbles: true, pointerType: "mouse" })); });
    await eventually(() => expect(rows[2].classList.contains("result-row--active")).toBe(true));
    expect(thumbnail).toHaveBeenCalledTimes(2);
  });

  it("does not light up or redraw a row a finger lands on", async () => {
    const thumbnail = vi.spyOn(api, "thumbnailUrl");
    await render();
    await eventually(() => expect(container?.querySelectorAll(".result-row")).toHaveLength(hits.length));
    thumbnail.mockClear();

    // A tap sends pointer (and compatibility mouse) events; only a real mouse moves the highlight.
    const rows = Array.from(container?.querySelectorAll(".result-row") ?? []);
    await act(async () => {
      rows[1].dispatchEvent(new PointerEvent("pointerover", { bubbles: true, pointerType: "touch" }));
      rows[1].dispatchEvent(new MouseEvent("mouseover", { bubbles: true }));
    });
    expect(rows[1].classList.contains("result-row--active")).toBe(false);
    expect(thumbnail).not.toHaveBeenCalled();
  });
});

describe("ArchivePage touch", () => {
  const opened: boolean[] = [];
  // ScreenLink prevents the browser's own navigation and drives the app's; a prevented title click is an open.
  const recordOpens = (event: Event) => { if ((event.target as Element).closest(".result-title")) opened.push(event.defaultPrevented); };
  const passportRow = () => Array.from(container?.querySelectorAll<HTMLElement>(".result-row") ?? []).find((row) => row.querySelector(".result-title")?.textContent?.startsWith("Passport"))!;
  const searchInput = () => container?.querySelector<HTMLInputElement>(".search-form input");

  function stubPointer(fine: boolean) {
    vi.stubGlobal("matchMedia", (query: string) => ({ matches: fine && query.includes("pointer: fine"), media: query, addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {} }));
  }

  beforeAll(() => document.addEventListener("click", recordOpens));
  afterAll(() => document.removeEventListener("click", recordOpens));
  afterEach(() => { opened.length = 0; });

  it("opens the document from the empty card space, the date and a field value", async () => {
    // Each open leaves the archive for the document, so every target gets a fresh page.
    for (const pick of [(row: HTMLElement) => row, (row: HTMLElement) => row.querySelector("time")!, (row: HTMLElement) => row.querySelector("dd")!]) {
      opened.length = 0;
      await render();
      await click(pick(passportRow()));
      expect(opened).toEqual([true]);
      await act(async () => root?.unmount());
      container?.remove();
      root = null;
      container = null;
    }
  });

  it("leaves a chip tap to the filter and does not open the document", async () => {
    await render();
    await click(passportRow().querySelector(".text-chip")!);
    await idle();
    expect(opened).toEqual([]);
  });

  it("does not open the document while text is selected, nor on a modified click", async () => {
    await render();
    const row = passportRow();
    const selection = vi.spyOn(window, "getSelection").mockReturnValue({ toString: () => "Alex" } as Selection);
    await click(row.querySelector("dd")!);
    selection.mockRestore();
    await act(async () => { row.querySelector("dd")!.dispatchEvent(new MouseEvent("click", { bubbles: true, ctrlKey: true })); });
    await act(async () => { row.querySelector("dd")!.dispatchEvent(new MouseEvent("click", { bubbles: true, button: 1 })); });
    expect(opened).toEqual([]);
  });

  it("ticks the row instead of opening it while selecting", async () => {
    await render();
    await click(button("Select"));
    await eventually(() => expect(checkboxes().length).toBeGreaterThan(1));
    const row = passportRow();
    const box = row.querySelector<HTMLInputElement>(".result-select input")!;
    await click(row.querySelector(".result-body")!);
    expect(box.checked).toBe(true);
    await click(row.querySelector("dd")!);
    expect(box.checked).toBe(false);
    expect(opened).toEqual([]);
  });

  it("highlights no row on render or after a tap, only after the arrow keys", async () => {
    await render();
    await eventually(() => expect(container?.querySelectorAll(".result-row")).toHaveLength(hits.length));
    const active = () => Array.from(container?.querySelectorAll(".result-row") ?? []).map((row) => row.classList.contains("result-row--active"));
    expect(active().some(Boolean)).toBe(false);

    const rows = Array.from(container?.querySelectorAll(".result-row") ?? []);
    await act(async () => { rows[1].dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, pointerType: "touch" })); });
    expect(active().some(Boolean)).toBe(false);

    const press = (key: string) => act(async () => { document.body.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true })); });
    // A tap leaves the roving position alone (it would redraw the page before the click).
    await press("ArrowDown");
    expect(active().indexOf(true)).toBe(0);
    await press("ArrowDown");
    expect(active().indexOf(true)).toBe(1);
    // A later tap puts the highlight away again.
    await act(async () => { rows[0].dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, pointerType: "touch" })); });
    expect(active().some(Boolean)).toBe(false);
  });

  it("does not focus the search field after a chip tap on a touch screen", async () => {
    stubPointer(false);
    await render();
    await click(passportRow().querySelector(".text-chip")!);
    await idle();
    expect(document.activeElement).not.toBe(searchInput());
  });

  it("focuses the search field after a chip click with a fine pointer", async () => {
    stubPointer(true);
    await render();
    await click(passportRow().querySelector(".text-chip")!);
    await eventually(() => expect(document.activeElement).toBe(searchInput()));
  });
});
