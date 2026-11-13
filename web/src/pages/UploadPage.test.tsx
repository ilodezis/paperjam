import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { I18nextProvider } from "react-i18next";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { Taxonomy } from "../api";
import { createI18n } from "../i18n";
import { ScreenHarness } from "../shell/testing";
import { runUpload } from "../upload";
import { UploadPage } from "./UploadPage";

// The page only needs the upload runner's progress callbacks; the request itself is out of scope here.
vi.mock("../upload", () => ({ runUpload: vi.fn() }));

// Seeded taxonomy: the page reads it from the query cache, so no request is made.
const taxonomy = {
  types: [{ id: "type-bank", key: "bank_statement", labels: { en: "Bank statement", ru: "Выписка" } }],
  people: [{ id: "person-alex", name: "Alex" }],
  correspondents: [],
  tags: [{ id: "tag-tax", name: "Tax" }],
} as unknown as Taxonomy;

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root | null = null;
let container: HTMLDivElement | null = null;
let queryClient: QueryClient | null = null;

afterEach(async () => {
  vi.clearAllMocks();
  if (root) await act(async () => root?.unmount());
  container?.remove();
  queryClient?.clear();
  root = null;
  container = null;
  queryClient = null;
});

async function renderPage() {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  queryClient.setQueryData(["taxonomy"], taxonomy);
  const i18n = createI18n("en");
  await act(async () => {
    root?.render(
      <QueryClientProvider client={queryClient!}>
        <I18nextProvider i18n={i18n}>
          <ScreenHarness><UploadPage /></ScreenHarness>
        </I18nextProvider>
      </QueryClientProvider>
    );
  });
}

function prefillCheckbox(name: string): HTMLInputElement {
  const option = Array.from(container?.querySelectorAll<HTMLLabelElement>(".upload-prefill-option") ?? [])
    .find((label) => label.textContent === name);
  const input = option?.querySelector<HTMLInputElement>("input");
  if (!input) throw new Error(`Prefill option ${name} not rendered`);
  return input;
}

function fileInput(): HTMLInputElement {
  const input = container?.querySelector<HTMLInputElement>("input[type=file]");
  if (!input) throw new Error("File input not rendered");
  return input;
}

function startButton(): HTMLButtonElement | null {
  return container?.querySelector<HTMLButtonElement>(".upload-start") ?? null;
}

/** Opens a SelectDropdown by its trigger and clicks the option with the given name. */
async function pick(trigger: Element | null | undefined, option: string) {
  if (!trigger) throw new Error("Dropdown not rendered");
  await act(async () => {
    trigger.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  const match = Array.from(trigger.closest(".select-dropdown-container")?.querySelectorAll<HTMLElement>('[role="option"]') ?? [])
    .find((node) => node.textContent?.trim() === option);
  if (!match) throw new Error(`Option ${option} not rendered`);
  await act(async () => {
    match.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

function stageTitle(): string | undefined {
  return container?.querySelector("#upload-queue-title")?.textContent ?? undefined;
}

async function chooseFile(name: string) {
  const input = fileInput();
  const file = new File(["synthetic"], name, { type: "application/pdf" });
  Object.defineProperty(input, "files", { value: [file], configurable: true });
  await act(async () => {
    input.dispatchEvent(new Event("change", { bubbles: true }));
  });
}

async function eventually(assertion: () => void) {
  let lastError: unknown;
  for (let attempt = 0; attempt < 40; attempt += 1) {
    try {
      await act(async () => { assertion(); });
      return;
    } catch (error) {
      lastError = error;
      await act(async () => { await new Promise((resolve) => setTimeout(resolve, 10)); });
    }
  }
  throw lastError;
}

describe("UploadPage", () => {
  it("renders no start button before any file is chosen", async () => {
    await renderPage();
    expect(startButton()).toBeNull();
  });

  it("accepts PDFs, images, plain text and Markdown in the file picker", async () => {
    await renderPage();
    const accept = fileInput().accept.split(",");
    expect(accept).toEqual(expect.arrayContaining(["application/pdf", "image/*", ".txt", ".md", "text/plain", "text/markdown"]));
  });

  it("shows the in-progress count instead of zero while an upload runs", async () => {
    vi.mocked(runUpload).mockImplementation((_file, _localOnly, onUpdate) => {
      onUpdate({ phase: "uploading" });
      return new Promise<void>(() => undefined);
    });
    await renderPage();
    await chooseFile("statement.pdf");
    await eventually(() => expect(startButton()?.textContent).toBe("Upload 1 document"));

    await act(async () => {
      startButton()?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });

    // Once the bytes are sent there is no upload button to press again; progress shows per file.
    await eventually(() => expect(startButton()).toBeNull());
    expect(runUpload).toHaveBeenCalledTimes(1);
  });

  it("refreshes the cached taxonomy and counts when a document finishes processing", async () => {
    // The new document brings a type and an organization: the cached lists that count them are
    // stale the moment it is ready, and the cache is kept on the device across reloads.
    vi.mocked(runUpload).mockImplementation(async (_file, _options, onUpdate) => {
      onUpdate({ phase: "queued", documentId: "doc-new" });
      onUpdate({ phase: "processing", documentId: "doc-new" });
      onUpdate({ phase: "ready", documentId: "doc-new" });
    });
    await renderPage();
    expect(queryClient?.getQueryState(["taxonomy"])?.isInvalidated).toBe(false);
    await chooseFile("receipt.pdf");
    await eventually(() => expect(startButton()).toBeTruthy());
    await act(async () => {
      startButton()?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await eventually(() => expect(queryClient?.getQueryState(["taxonomy"])?.isInvalidated).toBe(true));
  });

  it("passes the prefilled type and people to every waiting file on start", async () => {
    vi.mocked(runUpload).mockImplementation(() => new Promise<void>(() => undefined));
    await renderPage();
    await chooseFile("statement.pdf");

    await pick(container?.querySelector(".upload-prefill-type [role='combobox']"), "Bank statement");
    await act(async () => {
      prefillCheckbox("Alex").click();
    });
    await act(async () => {
      startButton()?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });

    await eventually(() => expect(runUpload).toHaveBeenCalledWith(
      expect.any(File),
      expect.objectContaining({ typeId: "type-bank", people: ["person-alex"] }),
      expect.any(Function),
    ));
  });

  it("shows the upload percentage from progress updates", async () => {
    vi.mocked(runUpload).mockImplementation((_file, _options, onUpdate) => {
      onUpdate({ phase: "uploading", progress: 0.42 });
      return new Promise<void>(() => undefined);
    });
    await renderPage();
    await chooseFile("statement.pdf");
    await act(async () => {
      startButton()?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });

    await eventually(() => expect(container?.querySelector(".upload-status")?.textContent).toContain("42%"));
    const bar = container?.querySelector<HTMLElement>(".upload-progress");
    expect(bar?.getAttribute("role")).toBe("progressbar");
    expect(bar?.getAttribute("aria-valuenow")).toBe("42");
    expect(bar?.querySelector<HTMLElement>("span")?.style.width).toBe("42%");
  });

  it("names the batch stage and never calls unsent files an archive", async () => {
    vi.mocked(runUpload).mockImplementation((_file, _options, onUpdate) => {
      onUpdate({ phase: "uploading" });
      return new Promise<void>(() => undefined);
    });
    await renderPage();
    expect(container?.querySelector("#upload-queue-title")).toBeNull();

    await chooseFile("statement.pdf");
    expect(stageTitle()).toBe("Ready to upload: 1");
    expect(container?.querySelector(".upload-summary")).toBeNull();
    expect(container?.textContent).not.toContain("Files in archive");

    await act(async () => {
      startButton()?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await eventually(() => expect(stageTitle()).toBe("Uploading / processing: 1"));
    expect(container?.querySelector(".upload-summary")).toBeNull();
  });

  it("marks the prefill section as optional information", async () => {
    await renderPage();
    await chooseFile("statement.pdf");
    const summary = container?.querySelector(".upload-prefill > summary")?.textContent ?? "";
    expect(summary).toContain("Add information");
    expect(summary).toContain("Optional");
  });

  it("applies the batch local-only setting and lets one file override it", async () => {
    vi.mocked(runUpload).mockImplementation((_file, _options, onUpdate) => {
      onUpdate({ phase: "uploading" });
      return new Promise<void>(() => undefined);
    });
    await renderPage();
    await chooseFile("a.pdf");
    await chooseFile("b.pdf");
    expect(container?.querySelectorAll(".upload-batch-privacy")).toHaveLength(1);
    expect(container?.querySelectorAll(".upload-item .local-only-control")).toHaveLength(0);

    await act(async () => {
      container?.querySelector<HTMLInputElement>(".upload-batch-privacy input")?.click();
    });
    const overrides = Array.from(container?.querySelectorAll<HTMLElement>(".upload-file-privacy") ?? []);
    await act(async () => {
      overrides[1]!.querySelector<HTMLButtonElement>("[role='combobox']")!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(overrides[1]!.querySelector('[role="option"]')?.textContent).toBe("Same as all files: processed on this server only");
    await act(async () => {
      Array.from(overrides[1]!.querySelectorAll<HTMLElement>('[role="option"]')).find((node) => node.textContent === "This file: outside services")!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await act(async () => {
      startButton()?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });

    const sent = vi.mocked(runUpload).mock.calls.map(([file, options]) => [file.name, options.localOnly]);
    expect(sent).toEqual([["a.pdf", true], ["b.pdf", false]]);
    await eventually(() => {
      const badges = Array.from(container?.querySelectorAll(".upload-local-badge") ?? []).map((badge) => badge.closest(".upload-item")?.querySelector("strong")?.textContent);
      expect(badges).toEqual(["a.pdf"]);
    });
  });

  it("asks about privacy once for a lone file", async () => {
    vi.mocked(runUpload).mockImplementation((_file, _options, onUpdate) => {
      onUpdate({ phase: "uploading" });
      return new Promise<void>(() => undefined);
    });
    await renderPage();
    await chooseFile("a.pdf");
    expect(container?.querySelectorAll(".upload-file-privacy")).toHaveLength(0);
    expect(container?.querySelector(".upload-batch-privacy")?.textContent).not.toContain("every file below");

    await act(async () => {
      container?.querySelector<HTMLInputElement>(".upload-batch-privacy input")?.click();
    });
    await act(async () => {
      startButton()?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(vi.mocked(runUpload).mock.calls.map(([, options]) => options.localOnly)).toEqual([true]);
  });

  it("links to the document once it is added and says the batch is Added", async () => {
    vi.mocked(runUpload).mockImplementation((_file, _options, onUpdate) => {
      onUpdate({ phase: "ready", documentId: "doc-1" });
      return Promise.resolve();
    });
    await renderPage();
    await chooseFile("statement.pdf");
    await act(async () => {
      startButton()?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });

    await eventually(() => expect(stageTitle()).toBe("Added: 1"));
    expect(container?.querySelector(".upload-status")?.textContent).toContain("Added");
    const link = container?.querySelector<HTMLAnchorElement>(".upload-item .upload-open");
    expect(link?.getAttribute("aria-label")).toBe("Open statement.pdf");
    expect(link?.getAttribute("href")).toBe("/documents/doc-1");
    expect(container?.querySelector(".upload-item strong")?.textContent).toBe("statement.pdf");
    expect(container?.querySelector(".upload-file-name")).toBeNull();
  });

  it("shows the extracted title as the row name with the file name beneath, and labels the link with it", async () => {
    vi.mocked(runUpload).mockImplementation((_file, _options, onUpdate) => {
      onUpdate({ phase: "queued", documentId: "doc-2" });
      onUpdate({ phase: "ready", documentId: "doc-2", title: "Bank statement, March 2026" });
      return Promise.resolve();
    });
    await renderPage();
    await chooseFile("scan-0042.pdf");
    await act(async () => {
      startButton()?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });

    await eventually(() => expect(container?.querySelector(".upload-item strong")?.textContent).toBe("Bank statement, March 2026"));
    expect(container?.querySelector(".upload-file-name")?.textContent).toBe("scan-0042.pdf");
    const link = container?.querySelector<HTMLAnchorElement>(".upload-item .upload-open");
    expect(link?.getAttribute("aria-label")).toBe("Open Bank statement, March 2026");
    expect(link?.getAttribute("href")).toBe("/documents/doc-2");
    expect(container?.querySelectorAll(".upload-item .upload-open")).toHaveLength(1);
  });

  it("reports a failed upload and lets the user retry it", async () => {
    vi.mocked(runUpload).mockImplementationOnce((_file, _options, onUpdate) => {
      onUpdate({ phase: "failed", code: "network" });
      return Promise.resolve();
    });
    await renderPage();
    await chooseFile("statement.pdf");
    await act(async () => {
      startButton()?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });

    await eventually(() => expect(stageTitle()).toBe("Not added: 1"));
    expect(container?.querySelector(".upload-summary")).toBeNull();
    expect(container?.querySelector(".upload-item--failed .upload-status")?.textContent).not.toBe("");
    const retry = container?.querySelector<HTMLButtonElement>(".upload-retry");
    expect(retry?.textContent).toBe("Try again");

    await act(async () => { retry?.click(); });
    expect(stageTitle()).toBe("Ready to upload: 1");
    expect(startButton()?.textContent).toBe("Upload 1 document");
    expect(container?.querySelector(".upload-item--waiting")).not.toBeNull();
  });

  it("offers no retry for a failure the same file would hit again", async () => {
    vi.mocked(runUpload).mockImplementationOnce((_file, _options, onUpdate) => {
      onUpdate({ phase: "failed", code: "unsupported_type" });
      return Promise.resolve();
    });
    await renderPage();
    await chooseFile("statement.pdf");
    await act(async () => {
      startButton()?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });

    await eventually(() => expect(stageTitle()).toBe("Not added: 1"));
    expect(container?.querySelector(".upload-item--failed .upload-status")?.textContent).not.toBe("");
    expect(container?.querySelector(".upload-retry")).toBeNull();
  });
});
