import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { I18nextProvider } from "react-i18next";
import { http, HttpResponse } from "msw";
import { setupServer } from "msw/node";
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import type { User } from "../api";
import { createI18n } from "../i18n";
import { handlers } from "../mocks/handlers";
import { LoginPage, consentReturn } from "./LoginPage";

const server = setupServer(...handlers);
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root | null = null;
let container: HTMLDivElement | null = null;
const get = vi.fn();

const buffer = (text: string): ArrayBuffer => new TextEncoder().encode(text).buffer as ArrayBuffer;

/** An assertion as a browser hands it over; `id` names the credential. */
function assertion(id: string) {
  return {
    id,
    rawId: buffer(atob(id.replace(/-/g, "+").replace(/_/g, "/"))),
    type: "public-key",
    authenticatorAttachment: "platform",
    getClientExtensionResults: () => ({}),
    response: { clientDataJSON: buffer("{}"), authenticatorData: buffer("data"), signature: buffer("sig"), userHandle: buffer("user") },
  };
}

function stubWebAuthn(conditional: boolean) {
  const signalUnknownCredential = vi.fn(() => Promise.resolve());
  vi.stubGlobal("PublicKeyCredential", {
    isConditionalMediationAvailable: () => Promise.resolve(conditional),
    signalUnknownCredential,
  });
  Object.defineProperty(navigator, "credentials", { value: { get }, configurable: true });
  return { signalUnknownCredential };
}

beforeAll(() => server.listen());
beforeEach(() => {
  get.mockReset();
});
afterEach(async () => {
  server.resetHandlers();
  vi.unstubAllGlobals();
  if (root) await act(async () => root?.unmount());
  container?.remove();
  root = null;
  container = null;
});
afterAll(() => server.close());

async function render(onUser: (user: User) => void) {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await act(async () => {
    root?.render(<I18nextProvider i18n={createI18n("en")}><LoginPage onUser={onUser} /></I18nextProvider>);
  });
}

function findButton(name: string): HTMLButtonElement | null {
  return Array.from(container?.querySelectorAll("button") ?? []).find((element) => element.textContent?.trim() === name) ?? null;
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

/** Records the JSON bodies posted to `path`, then lets the mock answer. */
function record(path: string): unknown[] {
  const bodies: unknown[] = [];
  server.events.on("request:start", async ({ request }) => {
    if (new URL(request.url).pathname === path) bodies.push(await request.clone().json());
  });
  return bodies;
}

describe("LoginPage passkey sign-in", () => {
  it("signs in with a passkey from the button", async () => {
    stubWebAuthn(false);
    const posted = record("/api/v1/auth/passkey");
    const onUser = vi.fn();
    get.mockResolvedValue(assertion("cGhvbmU"));
    await render(onUser);

    await eventually(() => expect(findButton("Sign in with a passkey")).not.toBeNull());
    const username = container!.querySelector<HTMLInputElement>("input[name=username]")!;
    expect(username.getAttribute("autocomplete")).toBe("username webauthn");
    await click(findButton("Sign in with a passkey")!);

    await eventually(() => expect(onUser).toHaveBeenCalledWith(expect.objectContaining({ username: "demo" })));
    expect(get).toHaveBeenCalledTimes(1);
    const request = get.mock.calls[0][0] as CredentialRequestOptions;
    expect("mediation" in request).toBe(false);
    expect(new TextDecoder().decode(request.publicKey!.challenge as ArrayBuffer)).toBe("challenge");
    expect(posted).toEqual([
      expect.objectContaining({
        ceremony: expect.stringMatching(/^ceremony-login-/),
        credential: expect.objectContaining({ id: "cGhvbmU", rawId: "cGhvbmU", response: expect.objectContaining({ signature: "c2ln" }) }),
      }),
    ]);
  });

  it("hides the button when the server has no passkeys configured", async () => {
    stubWebAuthn(false);
    server.use(http.post("/api/v1/auth/passkey/challenge", () => HttpResponse.json({ code: "passkeys_unavailable", message: "unavailable" }, { status: 503 })));
    await render(vi.fn());
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 50)); });
    expect(findButton("Sign in with a passkey")).toBeNull();
    expect(findButton("Sign in")).not.toBeNull();
  });

  it("hides the button where the browser has no WebAuthn", async () => {
    vi.stubGlobal("PublicKeyCredential", undefined);
    await render(vi.fn());
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 50)); });
    expect(findButton("Sign in with a passkey")).toBeNull();
    expect(container!.querySelector("input[name=username]")!.getAttribute("autocomplete")).toBe("username");
  });

  it("stays quiet when the sheet is closed and explains a removed passkey", async () => {
    const { signalUnknownCredential } = stubWebAuthn(false);
    const onUser = vi.fn();
    get.mockRejectedValueOnce(new DOMException("closed", "NotAllowedError"));
    get.mockResolvedValueOnce(assertion("Z29uZQ"));
    await render(onUser);
    await eventually(() => expect(findButton("Sign in with a passkey")).not.toBeNull());

    await click(findButton("Sign in with a passkey")!);
    await eventually(() => expect(findButton("Sign in with a passkey")?.disabled).toBe(false));
    expect(container!.querySelector("[role=alert]")).toBeNull();

    await click(findButton("Sign in with a passkey")!);
    await eventually(() => expect(container!.querySelector("[role=alert]")?.textContent).toBe("This passkey was removed from paperjam. Sign in with the password."));
    expect(signalUnknownCredential).toHaveBeenCalledWith({ rpId: "localhost", credentialId: "Z29uZQ" });
    expect(onUser).not.toHaveBeenCalled();
  });

  it("offers the passkey in the username autofill where supported", async () => {
    stubWebAuthn(true);
    const onUser = vi.fn();
    get.mockResolvedValue(assertion("cGhvbmU"));
    await render(onUser);

    await eventually(() => expect(onUser).toHaveBeenCalled());
    const request = get.mock.calls[0][0] as CredentialRequestOptions;
    expect(request.mediation).toBe("conditional");
    expect(request.signal).toBeInstanceOf(AbortSignal);
  });

  it("limits the challenge to the account whose name was typed", async () => {
    stubWebAuthn(false);
    const challenges = record("/api/v1/auth/passkey/challenge");
    get.mockResolvedValue(assertion("cGhvbmU"));
    const onUser = vi.fn();
    await render(onUser);
    await eventually(() => expect(findButton("Sign in with a passkey")).not.toBeNull());

    await type(container!.querySelector<HTMLInputElement>("input[name=username]")!, "demo");
    await eventually(() => expect(challenges).toContainEqual({ username: "demo" }));
    await click(findButton("Sign in with a passkey")!);

    await eventually(() => expect(onUser).toHaveBeenCalled());
    const request = get.mock.calls[0][0] as CredentialRequestOptions;
    expect(request.publicKey!.allowCredentials).toHaveLength(1);
  });
});

describe("LoginPage password sign-in", () => {
  afterEach(() => window.history.replaceState(null, "", "/"));

  async function signIn() {
    await type(container!.querySelector<HTMLInputElement>("input[name=username]")!, "demo");
    await type(container!.querySelector<HTMLInputElement>("input[name=password]")!, "secret");
    await act(async () => { container!.querySelector("form")!.requestSubmit(); });
  }

  it("returns to the application's consent page when one sent the person here", async () => {
    vi.stubGlobal("PublicKeyCredential", undefined);
    const consent = "/oauth/authorize?client_id=abc&state=xyz";
    window.history.replaceState(null, "", `/login?next=${encodeURIComponent(consent)}`);
    const onUser = vi.fn();
    const leave = vi.fn();
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    await act(async () => {
      root?.render(<I18nextProvider i18n={createI18n("en")}><LoginPage onUser={onUser} leave={leave} /></I18nextProvider>);
    });

    expect(container.textContent).toContain("Connecting an application");
    await signIn();
    await eventually(() => expect(leave).toHaveBeenCalledWith(consent));
    expect(onUser).not.toHaveBeenCalled();
  });

  it("opens the app as usual otherwise, and can show the password", async () => {
    vi.stubGlobal("PublicKeyCredential", undefined);
    const onUser = vi.fn();
    await render(onUser);
    expect(container!.textContent).not.toContain("Connecting an application");

    const password = container!.querySelector<HTMLInputElement>("input[name=password]")!;
    expect(password.type).toBe("password");
    await click(findButton("Show")!);
    expect(password.type).toBe("text");
    expect(findButton("Hide")!.getAttribute("aria-pressed")).toBe("true");

    await signIn();
    await eventually(() => expect(onUser).toHaveBeenCalledWith(expect.objectContaining({ username: "demo" })));
  });
});

describe("consentReturn", () => {
  const at = (next: string) => `https://archive.test/login?next=${encodeURIComponent(next)}`;

  it("accepts this origin's consent page with its query", () => {
    expect(consentReturn(at("/oauth/authorize?client_id=a&state=b"))).toBe("/oauth/authorize?client_id=a&state=b");
  });

  it("refuses anything that would make sign-in an open redirect", () => {
    expect(consentReturn("https://archive.test/login")).toBeNull();
    expect(consentReturn(at("https://evil.test/oauth/authorize?x=1"))).toBeNull();
    expect(consentReturn(at("//evil.test/oauth/authorize?x=1"))).toBeNull();
    expect(consentReturn(at("/\\evil.test/oauth/authorize"))).toBeNull();
    expect(consentReturn(at("/settings"))).toBeNull();
    expect(consentReturn(at("/oauth/authorize/../../settings"))).toBeNull();
    expect(consentReturn(at("oauth/authorize?x=1"))).toBeNull();
  });
});
