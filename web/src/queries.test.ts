import { QueryClient, queryOptions } from "@tanstack/react-query";
import { afterEach, describe, expect, it, vi } from "vitest";

import { api, type DocumentView, type Hit } from "./api";
import { ensureFresh, invalidateArchive, statsQuery, taxonomyQuery, trashQuery, watchIngest } from "./queries";

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

function setup(staleTime = 60_000) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const queryFn = vi.fn<() => Promise<string>>();
  const options = queryOptions({ queryKey: ["probe"], queryFn, staleTime });
  return { client, queryFn, options };
}

describe("ensureFresh", () => {
  it("fetches and waits when nothing is cached", async () => {
    const { client, queryFn, options } = setup();
    queryFn.mockResolvedValue("first");
    await ensureFresh(client, options, 600);
    expect(client.getQueryData(options.queryKey)).toBe("first");
  });

  it("does not touch the network while the cached copy is fresh", async () => {
    const { client, queryFn, options } = setup();
    client.setQueryData(options.queryKey, "cached");
    await ensureFresh(client, options, 600);
    expect(queryFn).not.toHaveBeenCalled();
  });

  it("waits for the fresh answer instead of drawing a stale copy first", async () => {
    const { client, queryFn, options } = setup(0);
    client.setQueryData(options.queryKey, "stale");
    queryFn.mockResolvedValue("fresh");
    await ensureFresh(client, options, 600);
    expect(client.getQueryData(options.queryKey)).toBe("fresh");
  });

  it("gives up waiting on a slow network and keeps the stale copy", async () => {
    vi.useFakeTimers();
    const { client, queryFn, options } = setup(0);
    client.setQueryData(options.queryKey, "stale");
    queryFn.mockReturnValue(new Promise(() => {}));
    const done = ensureFresh(client, options, 600);
    await vi.advanceTimersByTimeAsync(600);
    await done;
    expect(client.getQueryData(options.queryKey)).toBe("stale");
  });

  it("keeps the stale copy when the refresh fails (offline)", async () => {
    const { client, queryFn, options } = setup(0);
    client.setQueryData(options.queryKey, "stale");
    queryFn.mockRejectedValue(new Error("offline"));
    await ensureFresh(client, options, 600);
    expect(client.getQueryData(options.queryKey)).toBe("stale");
  });
});

describe("the archive's cached lists", () => {
  const document = (status: string) => ({ id: "doc-1", status }) as unknown as DocumentView;
  const KEYS = [["search", "x", "relevance"], ["taxonomy"], ["stats"], ["trash"], ["document", "doc-1", 1]];

  function seeded() {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
    for (const key of KEYS) client.setQueryData(key, key[0] === "document" ? document("processing") : {});
    return client;
  }
  const invalidated = (client: QueryClient) => KEYS.filter((key) => client.getQueryState(key)?.isInvalidated).map((key) => key[0]);

  it("goes stale quickly: a counted list is not kept for minutes on a device that reloads", () => {
    // The cache is written to the device, so what is stale by the clock is asked for again on reload.
    expect(taxonomyQuery.staleTime).toBeLessThanOrEqual(60_000);
    expect(statsQuery.staleTime).toBeLessThanOrEqual(60_000);
  });

  it("invalidates every list built from the archive's contents together", async () => {
    const client = seeded();
    await invalidateArchive(client);
    expect(invalidated(client)).toEqual(["search", "taxonomy", "stats", "trash", "document"]);
  });

  it("invalidates them when a followed document settles, and not before", () => {
    const client = seeded();
    const stop = watchIngest(client);
    client.setQueryData(["document", "doc-1", 1], document("processing"));
    expect(invalidated(client)).toEqual([]);

    client.setQueryData(["document", "doc-1", 1], document("ready"));
    expect(invalidated(client)).toEqual(["search", "taxonomy", "stats", "trash", "document"]);
    stop();
  });

  it("also does so for a document that failed, once", () => {
    const client = seeded();
    const stop = watchIngest(client);
    client.setQueryData(["document", "doc-1", 1], document("queued"));
    client.setQueryData(["document", "doc-1", 1], document("failed"));
    expect(invalidated(client)).toContain("taxonomy");
    // Settled documents that are only refreshed do not set it off again.
    client.setQueryData(["taxonomy"], {});
    client.setQueryData(["document", "doc-1", 1], document("failed"));
    expect(invalidated(client)).not.toContain("taxonomy");
    stop();
  });
});

describe("trashQuery", () => {
  const hit = (id: string, trashed: boolean): Hit => ({
    id,
    title: id,
    type_key: null,
    document_date: null,
    organizations: [],
    people: [],
    key_fields: {},
    snippet: "",
    trashed,
  });

  it("makes zero document detail requests when no search hits are trashed", async () => {
    vi.spyOn(api, "search").mockResolvedValue({
      hits: [hit("doc-1", false), hit("doc-2", false)],
      total: 2,
      next_cursor: null,
    });
    const docSpy = vi.spyOn(api, "document");
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const result = await client.fetchQuery(trashQuery);
    expect(result).toEqual([]);
    expect(docSpy).not.toHaveBeenCalled();
  });

  it("fetches details only for search hits marked trashed", async () => {
    vi.spyOn(api, "search").mockResolvedValue({
      hits: [hit("doc-live", false), hit("doc-trashed", true)],
      total: 2,
      next_cursor: null,
    });
    const docSpy = vi.spyOn(api, "document").mockResolvedValue({ id: "doc-trashed", trashed_at: "2026-11-20T00:00:00Z" } as DocumentView);
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const result = await client.fetchQuery(trashQuery);
    expect(docSpy).toHaveBeenCalledTimes(1);
    expect(docSpy).toHaveBeenCalledWith("doc-trashed");
    expect(result.map((d) => d.id)).toEqual(["doc-trashed"]);
  });
});
