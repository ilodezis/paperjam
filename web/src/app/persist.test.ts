import type { PersistedClient } from "@tanstack/react-query-persist-client";
import { QueryClient } from "@tanstack/react-query";
import { afterEach, describe, expect, it, vi } from "vitest";

import { createPersister, shouldPersist } from "./persist";

const client = (marker: string): PersistedClient => ({ timestamp: 1, buster: marker, clientState: { mutations: [], queries: [] } });

function memory() {
  const data = new Map<string, PersistedClient>();
  return {
    data,
    set: vi.fn(async (key: string, value: PersistedClient) => void data.set(key, value)),
    get: vi.fn(async (key: string) => data.get(key)),
    del: vi.fn(async (key: string) => void data.delete(key)),
  };
}

afterEach(() => vi.useRealTimers());

describe("persisted query cache", () => {
  it("writes the newest state once per interval, not once per change", async () => {
    vi.useFakeTimers();
    const storage = memory();
    const persister = createPersister(storage, 1000);
    void persister.persistClient(client("a"));
    void persister.persistClient(client("b"));
    void persister.persistClient(client("c"));
    expect(storage.set).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1000);
    expect(storage.set).toHaveBeenCalledTimes(1);
    expect((await persister.restoreClient())?.buster).toBe("c");
  });

  it("does not bring back what was removed from a write still waiting", async () => {
    vi.useFakeTimers();
    const storage = memory();
    const persister = createPersister(storage, 1000);
    void persister.persistClient(client("a"));
    await persister.removeClient();
    await vi.advanceTimersByTimeAsync(2000);
    expect(storage.set).not.toHaveBeenCalled();
    expect(await persister.restoreClient()).toBeUndefined();
  });

  it("opens from the network when storage refuses", async () => {
    const storage = {
      set: vi.fn().mockRejectedValue(new Error("full")),
      get: vi.fn().mockRejectedValue(new Error("private window")),
      del: vi.fn().mockRejectedValue(new Error("nope")),
    };
    const persister = createPersister(storage, 0);
    await expect(persister.restoreClient()).resolves.toBeUndefined();
    await expect(persister.removeClient()).resolves.toBeUndefined();
  });
});

describe("what leaves memory", () => {
  const queryFor = (key: unknown[], data: unknown) => {
    const qc = new QueryClient();
    qc.setQueryData(key, data);
    return qc.getQueryCache().find({ queryKey: key })!;
  };

  it("keeps documents and the archive, never credentials or the administrators' lists", () => {
    expect(shouldPersist(queryFor(["search", "", "relevance"], {}))).toBe(true);
    expect(shouldPersist(queryFor(["document", "a", 1], {}))).toBe(true);
    expect(shouldPersist(queryFor(["session"], {}))).toBe(true);
    expect(shouldPersist(queryFor(["tokens"], []))).toBe(false);
    expect(shouldPersist(queryFor(["users"], []))).toBe(false);
    expect(shouldPersist(queryFor(["passkeys"], {}))).toBe(false);
    expect(shouldPersist(queryFor(["email-settings"], {}))).toBe(false);
  });
});
