import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { I18nextProvider } from "react-i18next";
import { http, HttpResponse } from "msw";
import { setupServer } from "msw/node";
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import { api } from "../api";
import { createI18n } from "../i18n";
import { handlers } from "../mocks/handlers";
import { PasskeysSection } from "./PasskeysSection";
import { clearToasts, getToastsSnapshot } from "./controls/toast";

const server = setupServer(...handlers);
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root | null = null;
let container: HTMLDivElement | null = null;
const create = vi.fn();

const buffer = (text: string): ArrayBuffer => new TextEncoder().encode(text).buffer as ArrayBuffer;

/** A new credential as the browser hands it over. */
function attestation(id: string) {
  return {
    id,
    rawId: buffer(atob(id)),
    type: "public-key",
    authenticatorAttachment: "platform",
    getClientExtensionResults: () => ({ credProps: { rk: true } }),
    response: { clientDataJSON: buffer("{}"), attestationObject: buffer("att"), getTransports: () => ["internal"] },
  };
}

beforeAll(() => server.listen());
beforeEach(() => {
  create.mockReset();
  clearToasts();
  Object.defineProperty(navigator, "credentials", { value: { get: vi.fn(), create }, configurable: true });
});
afterEach(async () => {
  server.resetHandlers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  if (root) await act(async () => root?.unmount());
  container?.remove();
  root = null;
  container = null;
});
afterAll(() => server.close());

async function render() {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  await act(async () => {
    root?.render(
      <QueryClientProvider client={queryClient}>
        <I18nextProvider i18n={createI18n("en")}><PasskeysSection /></I18nextProvider>
      </QueryClientProvider>,
    );
  });
}

function findButton(name: string): HTMLButtonElement | null {
  return Array.from(container?.querySelectorAll("button") ?? []).find((element) => element.textContent?.trim() === name || element.getAttribute("aria-label") === name) ?? null;
}

async function click(element: Element) {
  await act(async () => { element.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
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

describe("PasskeysSection", () => {
  it("lists passkeys with their dates", async () => {
    vi.stubGlobal("PublicKeyCredential", {});
    await render();
    await eventually(() => expect(container?.textContent).toContain("Demo phone"));
    expect(container?.textContent).toContain("Created Feb 3, 2026");
    expect(container?.textContent).toContain("Last sign-in Feb 14, 2026");
  });

  it("adds a passkey with a label in two taps", async () => {
    vi.stubGlobal("PublicKeyCredential", {});
    const posted: unknown[] = [];
    server.events.on("request:start", async ({ request }) => {
      if (new URL(request.url).pathname === "/api/v1/passkeys" && request.method === "POST") posted.push(await request.clone().json());
    });
    create.mockResolvedValue(attestation("bGFwdG9w"));
    await render();
    await eventually(() => expect(findButton("Add a passkey")?.disabled).toBe(false));

    await click(findButton("Add a passkey")!);
    await eventually(() => expect(findButton("Create passkey")?.disabled).toBe(false));
    await type(container!.querySelector<HTMLInputElement>("#passkey-label")!, "Laptop");
    await click(findButton("Create passkey")!);

    await eventually(() => expect(container?.textContent).toContain("Laptop"));
    expect(container?.textContent).toContain("Not used yet");
    expect(getToastsSnapshot().map((t) => t.message)).toContain("Passkey added");
    const options = (create.mock.calls[0][0] as CredentialCreationOptions).publicKey!;
    expect(new TextDecoder().decode(options.challenge as ArrayBuffer)).toBe("challenge");
    expect(options.user.id).toBeInstanceOf(ArrayBuffer);
    expect(options.excludeCredentials?.[0]?.id).toBeInstanceOf(ArrayBuffer);
    expect("hints" in options).toBe(false);
    expect(posted).toEqual([
      expect.objectContaining({
        ceremony: expect.stringMatching(/^ceremony-register-/),
        label: "Laptop",
        credential: expect.objectContaining({ id: "bGFwdG9w", response: expect.objectContaining({ attestationObject: "YXR0", transports: ["internal"] }) }),
      }),
    ]);
  });

  it("names an unnamed passkey and stays quiet when the sheet is closed", async () => {
    vi.stubGlobal("PublicKeyCredential", {});
    create.mockRejectedValueOnce(new DOMException("closed", "NotAllowedError"));
    create.mockRejectedValueOnce(new DOMException("exists", "InvalidStateError"));
    await render();
    await eventually(() => expect(findButton("Add a passkey")?.disabled).toBe(false));
    await click(findButton("Add a passkey")!);
    await eventually(() => expect(findButton("Create passkey")?.disabled).toBe(false));

    await click(findButton("Create passkey")!);
    await eventually(() => expect(findButton("Create passkey")?.disabled).toBe(false));
    expect(getToastsSnapshot()).toEqual([]);

    await click(findButton("Create passkey")!);
    await eventually(() => expect(getToastsSnapshot().map((t) => t.message)).toEqual(["This passkey is already added."]));
    // Each tap used a challenge of its own.
    expect(create).toHaveBeenCalledTimes(2);
    const first = (create.mock.calls[0][0] as CredentialCreationOptions).publicKey!;
    const second = (create.mock.calls[1][0] as CredentialCreationOptions).publicKey!;
    expect(first).not.toBe(second);
  });

  it("removes a passkey after confirmation and tells the keychain", async () => {
    const signalAllAcceptedCredentials = vi.fn(() => Promise.resolve());
    vi.stubGlobal("PublicKeyCredential", { signalAllAcceptedCredentials });
    vi.spyOn(window, "confirm").mockReturnValue(true);
    const before = (await api.passkeys()).passkeys;
    const target = before.find((p) => p.label === "Demo phone")!;
    await render();
    await eventually(() => expect(findButton("Remove Demo phone")).not.toBeNull());

    await click(findButton("Remove Demo phone")!);
    await eventually(() => expect(findButton("Remove Demo phone")).toBeNull());
    expect((await api.passkeys()).passkeys.some((p) => p.id === target.id)).toBe(false);
    expect(signalAllAcceptedCredentials).toHaveBeenCalledWith({
      rpId: "localhost",
      userId: "AYAAAAAAcACAAAAAAAAAAQ",
      allAcceptedCredentialIds: before.filter((p) => p.id !== target.id).map((p) => p.credential_id),
    });
  });

  it("keeps a passkey when removal is not confirmed", async () => {
    vi.stubGlobal("PublicKeyCredential", {});
    vi.spyOn(window, "confirm").mockReturnValue(false);
    await render();
    await eventually(() => expect(findButton("Remove Laptop")).not.toBeNull());
    await click(findButton("Remove Laptop")!);
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 30)); });
    expect(findButton("Remove Laptop")).not.toBeNull();
  });

  it("explains what is missing when the server has no relying party", async () => {
    vi.stubGlobal("PublicKeyCredential", {});
    server.use(http.get("/api/v1/passkeys", () => HttpResponse.json({ available: false, rp_id: null, user_handle: "AQ", passkeys: [] })));
    await render();
    await eventually(() => expect(container?.textContent).toContain("PAPERJAM_PUBLIC_URL"));
    expect(findButton("Add a passkey")).toBeNull();
  });

  it("says so when the browser cannot create passkeys", async () => {
    vi.stubGlobal("PublicKeyCredential", undefined);
    await render();
    await eventually(() => expect(container?.textContent).toContain("This browser can't create passkeys."));
    expect(findButton("Add a passkey")).toBeNull();
  });
});
