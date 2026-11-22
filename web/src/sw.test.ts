// @vitest-environment node
import { describe, expect, it } from "vitest";

import swScript from "../public/sw.js?raw";

const ORIGIN = "https://archive.example";

type FakeResponse = {
  ok: boolean;
  status: number;
  type: string;
  headers: Headers;
  body: string;
  clone(): FakeResponse;
};

function response(body: string, cacheControl = "no-cache", status = 200): FakeResponse {
  const res: FakeResponse = {
    ok: status >= 200 && status < 300,
    status,
    type: "basic",
    headers: new Headers({ "Cache-Control": cacheControl }),
    body,
    clone: () => res,
  };
  return res;
}

type FetchEvent = {
  request: { method: string; url: string; mode: string };
  respondWith(answer: Promise<FakeResponse>): void;
};

/** Runs `public/sw.js` against an in-memory cache and a scripted network. */
function worker(network: (path: string) => FakeResponse | Error) {
  const handlers: Record<string, (event: FetchEvent) => void> = {};
  const stored = new Map<string, FakeResponse>();
  const path = (req: string | { url: string }) =>
    new URL(typeof req === "string" ? req : req.url, ORIGIN).pathname;
  const cache = {
    put: async (req: string | { url: string }, res: FakeResponse) => void stored.set(path(req), res),
    match: async (req: string | { url: string }) => stored.get(path(req)),
    addAll: async () => undefined,
  };
  const caches = {
    open: async () => cache,
    match: async (req: string | { url: string }) => stored.get(path(req)),
    keys: async () => [],
    delete: async () => true,
  };
  const self = {
    location: { origin: ORIGIN },
    addEventListener: (type: string, handler: (event: FetchEvent) => void) => {
      handlers[type] = handler;
    },
  };
  const fetch = async (req: { url: string }) => {
    const answer = network(path(req));
    if (answer instanceof Error) throw answer;
    return answer;
  };
  new Function("self", "caches", "fetch", swScript)(self, caches, fetch);

  async function get(url: string, mode = "navigate"): Promise<FakeResponse | undefined> {
    let answer: Promise<FakeResponse> | undefined;
    handlers.fetch({
      request: { method: "GET", url: new URL(url, ORIGIN).href, mode },
      respondWith: (p) => {
        answer = p;
      },
    });
    const res = answer ? await answer : undefined;
    // Let the worker's cache writes settle.
    await new Promise((resolve) => setTimeout(resolve, 0));
    return res;
  }
  return { get, stored };
}

describe("service worker caching", () => {
  it("leaves shared documents, OAuth, calendars and the API to the network", async () => {
    const sw = worker((p) => response(`page ${p}`, "no-store"));
    for (const url of ["/s/secret-token", "/oauth/authorize?client_id=x", "/calendar/feed.ics", "/api/v1/session"]) {
      expect(await sw.get(url)).toBeUndefined();
      expect(await sw.get(url, "no-cors")).toBeUndefined();
    }
    expect(sw.stored.size).toBe(0);
  });

  it("stores only the app shell from navigations", async () => {
    const sw = worker((p) => response(`page ${p}`));
    expect((await sw.get("/documents/42"))?.body).toBe("page /documents/42");
    expect(sw.stored.size).toBe(0);
    await sw.get("/");
    expect(sw.stored.get("/index.html")?.body).toBe("page /");
    expect(sw.stored.get("/")?.body).toBe("page /");
  });

  it("does not store responses the server marks no-store or private", async () => {
    const sw = worker((p) => response(`page ${p}`, p === "/" ? "no-store" : "private, max-age=60"));
    await sw.get("/");
    await sw.get("/assets/app.js", "no-cors");
    expect(sw.stored.size).toBe(0);
  });

  it("caches static assets and serves the shell offline", async () => {
    let online = true;
    const sw = worker((p) => (online ? response(`file ${p}`, "public, max-age=31536000, immutable") : new Error("offline")));
    await sw.get("/assets/app.js", "no-cors");
    expect(sw.stored.has("/assets/app.js")).toBe(true);
    await sw.get("/");
    online = false;
    expect((await sw.get("/documents/7"))?.body).toBe("file /");
  });
});
