import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { I18nextProvider } from "react-i18next";
import { setupServer } from "msw/node";
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { createI18n } from "../i18n";
import { handlers } from "../mocks/handlers";
import { CalendarFeedSection } from "./CalendarFeedSection";
import { clearToasts } from "./controls/toast";

const server = setupServer(...handlers);
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root | null = null;
let container: HTMLDivElement | null = null;

beforeAll(() => server.listen());
afterEach(async () => {
  server.resetHandlers();
  clearToasts();
  if (root) await act(async () => root?.unmount());
  container?.remove();
  root = null;
  container = null;
});
afterAll(() => server.close());

function findButton(name: string): HTMLButtonElement | null {
  return Array.from(container?.querySelectorAll("button") ?? []).find((element) => element.textContent?.trim() === name || element.getAttribute("aria-label") === name) ?? null;
}

async function click(element: Element) {
  await act(async () => { element.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
}

async function eventually(assertion: () => void, timeout = 1500) {
  const started = Date.now();
  for (;;) {
    try {
      await act(async () => { assertion(); });
      return;
    } catch (error) {
      if (Date.now() - started > timeout) throw error;
      await act(async () => { await new Promise((resolve) => setTimeout(resolve, 10)); });
    }
  }
}

describe("CalendarFeedSection", () => {
  it("asks rotate and revoke confirmations with an English Cancel button", async () => {
    const confirm = vi.fn().mockResolvedValue(false);
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <I18nextProvider i18n={createI18n("en")}><CalendarFeedSection confirm={confirm} onError={() => {}} /></I18nextProvider>
        </QueryClientProvider>,
      );
    });

    await eventually(() => expect(findButton("Create link")).not.toBeNull());
    await click(findButton("Create link")!);
    await eventually(() => expect(findButton("New link")).not.toBeNull());

    await click(findButton("New link")!);
    await click(findButton("Revoke calendar link")!);

    expect(confirm).toHaveBeenCalledTimes(2);
    expect(confirm.mock.calls.map(([options]) => options.cancelLabel)).toEqual(["Cancel", "Cancel"]);
  });
});
