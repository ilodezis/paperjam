import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { I18nextProvider } from "react-i18next";
import { http, HttpResponse } from "msw";
import { setupServer } from "msw/node";
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";

// Nothing to do once signed in.
const noop = () => undefined;
import { createI18n } from "../i18n";
import { handlers } from "../mocks/handlers";
import { AuthPage } from "./AuthPage";
import { SetupPage } from "./SetupPage";

const server = setupServer(...handlers);
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root | null = null;
let container: HTMLDivElement | null = null;

beforeAll(() => server.listen());
afterEach(async () => {
  server.resetHandlers();
  if (root) await act(async () => root?.unmount());
  container?.remove();
  root = null;
  container = null;
});
afterAll(() => server.close());

async function render(node: React.ReactNode) {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await act(async () => {
    root?.render(<I18nextProvider i18n={createI18n("en")}>{node}</I18nextProvider>);
  });
}

async function type(element: HTMLInputElement, value: string) {
  Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(element, value);
  await act(async () => { element.dispatchEvent(new Event("input", { bubbles: true })); });
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

async function fill(code: string) {
  await type(container!.querySelector<HTMLInputElement>("input[name=code]")!, code);
  await type(container!.querySelector<HTMLInputElement>("input[name=username]")!, "alice");
  await type(container!.querySelector<HTMLInputElement>("input[name=password]")!, "correct horse battery");
  await act(async () => { container!.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true })); });
}

describe("SetupPage", () => {
  it("creates the administrator with the setup code", async () => {
    const onUser = vi.fn();
    await render(<SetupPage onUser={onUser} />);
    await fill("aaaa-bbbb-cccc");
    await eventually(() => expect(onUser).toHaveBeenCalledWith(expect.objectContaining({ username: "alice" })));
  });

  it("explains a wrong code and stays on the page", async () => {
    const onUser = vi.fn();
    await render(<SetupPage onUser={onUser} />);
    await fill("0000-0000-0000");
    await eventually(() => expect(container!.querySelector("[role=alert]")?.textContent).toBe("That is not the setup code from the server log."));
    expect(onUser).not.toHaveBeenCalled();
  });
});

describe("AuthPage", () => {
  it("shows the sign-in when the instance is set up", async () => {
    await render(<AuthPage onUser={noop} />);
    await eventually(() => expect(container!.querySelector("input[name=username]")).not.toBeNull());
    expect(container!.querySelector("input[name=code]")).toBeNull();
  });

  it("shows the setup while there is no user", async () => {
    server.use(http.get("/api/v1/setup", () => HttpResponse.json({ needed: true })));
    await render(<AuthPage onUser={noop} />);
    await eventually(() => expect(container!.querySelector("input[name=code]")).not.toBeNull());
  });
});
