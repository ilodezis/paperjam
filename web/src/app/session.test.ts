import { afterEach, describe, expect, it, vi } from "vitest";

const logout = vi.fn();
const removeClient = vi.fn(async () => undefined);
const clearCachedPages = vi.fn(async () => undefined);

vi.mock("../api", () => ({ api: { logout: () => logout() } }));
vi.mock("./persist", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./persist")>()),
  persister: { removeClient: () => removeClient() },
}));
vi.mock("../pwa", () => ({ clearCachedPages: () => clearCachedPages() }));

const { signOut } = await import("./session");
const { queryClient } = await import("./queryClient");
const { sessionQuery } = await import("../queries");

afterEach(() => {
  vi.clearAllMocks();
  queryClient.clear();
  window.localStorage.clear();
});

function signedInDevice() {
  window.localStorage.setItem("paperjam:signed-in", "1");
  queryClient.setQueryData(sessionQuery.queryKey, {
    user: { id: "u1", username: "alice", display_name: "Alice", locale: "en", is_admin: true },
  });
  queryClient.setQueryData(["documents", "list"], { items: ["secret"] });
}

function expectForgotten() {
  expect(window.localStorage.getItem("paperjam:signed-in")).toBeNull();
  expect(queryClient.getQueryData(["documents", "list"])).toBeUndefined();
  expect(queryClient.getQueryData(sessionQuery.queryKey)).toEqual({ user: null });
  expect(removeClient).toHaveBeenCalledOnce();
  expect(clearCachedPages).toHaveBeenCalledOnce();
}

describe("signing out", () => {
  it("forgets the user on this device after the server signs out", async () => {
    signedInDevice();
    logout.mockResolvedValue(undefined);
    await signOut();
    expectForgotten();
  });

  it("forgets the user on this device even when the server cannot be reached", async () => {
    signedInDevice();
    logout.mockRejectedValue(new TypeError("Failed to fetch"));
    await expect(signOut()).rejects.toThrow("Failed to fetch");
    expectForgotten();
  });
});
