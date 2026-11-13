import { afterEach, describe, expect, it, vi } from "vitest";

import { api, ApiError } from "./api";

afterEach(() => {
  vi.unstubAllGlobals();
});

function response(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

describe("API client", () => {
  it("uses same-origin credentials and JSON request bodies", async () => {
    const fetchMock = vi.fn().mockResolvedValue(response(200, { user: { id: "u1" } }));
    vi.stubGlobal("fetch", fetchMock);

    await api.login("demo", "secret");

    expect(fetchMock).toHaveBeenCalledWith("/api/v1/auth/login", expect.objectContaining({
      method: "POST",
      credentials: "same-origin",
      body: JSON.stringify({ username: "demo", password: "secret" }),
    }));
  });

  it("returns undefined for a successful empty response", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(null, { status: 204 })));
    await expect(api.logout()).resolves.toBeUndefined();
  });

  it("preserves stable error codes and duplicate document details", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(response(409, {
      code: "duplicate",
      message: "Already archived",
      document_id: "doc-1",
      details: { document_id: "doc-1" },
    })));

    const error = await api.session().catch((value: unknown) => value);
    expect(error).toBeInstanceOf(ApiError);
    expect(error).toMatchObject({
      status: 409,
      code: "duplicate",
      documentId: "doc-1",
      details: { document_id: "doc-1" },
    });
  });

  it("handles an empty or non-JSON error response", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("unavailable", { status: 503, statusText: "Unavailable" })));
    await expect(api.session()).rejects.toMatchObject({ status: 503, code: "unknown", message: "Unavailable" });
  });

  it("sends search sort, cursor and structured filters", async () => {
    const fetchMock = vi.fn().mockResolvedValue(response(200, { hits: [], total: 0, next_cursor: null }));
    vi.stubGlobal("fetch", fetchMock);

    await api.search({ query: "type:passport", sort: "date_desc", cursor: "next", includeTrashed: false });

    expect(fetchMock).toHaveBeenCalledWith("/api/v1/search", expect.objectContaining({
      method: "POST",
      body: JSON.stringify({
        query: "type:passport",
        filters: { include_trashed: false },
        sort: "date_desc",
        limit: 20,
        cursor: "next",
      }),
    }));
  });

  it("loads the shared taxonomy", async () => {
    const fetchMock = vi.fn().mockResolvedValue(response(200, { types: [], people: [], correspondents: [], tags: [] }));
    vi.stubGlobal("fetch", fetchMock);
    await api.taxonomy();
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/taxonomy", expect.objectContaining({ credentials: "same-origin" }));
  });

  it("loads a document with the requested text page", async () => {
    const fetchMock = vi.fn().mockResolvedValue(response(200, { id: "doc-1", status: "ready" }));
    vi.stubGlobal("fetch", fetchMock);
    await api.document("doc-1", 2);
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/documents/doc-1?text_page=2", expect.objectContaining({ credentials: "same-origin" }));
  });

  it("patches metadata with typed field values", async () => {
    const fetchMock = vi.fn().mockResolvedValue(response(200, { id: "doc-1", status: "ready" }));
    vi.stubGlobal("fetch", fetchMock);
    const patch = { title: "Edited", fields: { total: { kind: "money", value: { amount: "10.00", currency: "EUR" } } } } as const;
    await api.patchDocument("doc-1", patch);
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/documents/doc-1", expect.objectContaining({ method: "PATCH", body: JSON.stringify(patch) }));
  });

  it("patches metadata with organizations list", async () => {
    const fetchMock = vi.fn().mockResolvedValue(response(200, { id: "doc-1", status: "ready" }));
    vi.stubGlobal("fetch", fetchMock);
    const patch = { organizations: [{ id: "corr-1", role: "issued_by" as const }] };
    await api.patchDocument("doc-1", patch);
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/documents/doc-1", expect.objectContaining({ method: "PATCH", body: JSON.stringify(patch) }));
  });

  it("queues local reprocessing and creates a short-lived download link", async () => {
    const fetchMock = vi.fn()
      .mockResolvedValueOnce(response(202, { id: "doc-1", status: "queued" }))
      .mockResolvedValueOnce(response(200, { url: "https://example.invalid/api/v1/dl/synthetic", expires_at: "2026-01-01T00:10:00Z" }));
    vi.stubGlobal("fetch", fetchMock);
    await api.reprocess("doc-1", false);
    await api.createDownloadLink("doc-1");
    expect(fetchMock).toHaveBeenNthCalledWith(1, "/api/v1/documents/doc-1/reprocess", expect.objectContaining({ method: "POST", body: JSON.stringify({ local: true, llm: false }) }));
    expect(fetchMock).toHaveBeenNthCalledWith(2, "/api/v1/documents/doc-1/download-link", expect.objectContaining({ method: "POST" }));
  });

  it("manages people, aliases and duplicate merges", async () => {
    const fetchMock = vi.fn().mockResolvedValue(response(200, {}));
    vi.stubGlobal("fetch", fetchMock);

    await api.createPerson({ name: "Alex", aliases: ["A"], notes: "Family" });
    await api.updatePerson("person-1", { name: "Alexander", notes: "Updated" });
    await api.addPersonAlias("person-1", "Sasha");
    await api.removePersonAlias("person-1", "A & S");
    await api.mergePeople("person-1", "person-2");
    await api.deletePerson("person-1");

    expect(fetchMock).toHaveBeenNthCalledWith(1, "/api/v1/people", expect.objectContaining({ method: "POST", body: JSON.stringify({ name: "Alex", aliases: ["A"], notes: "Family" }) }));
    expect(fetchMock).toHaveBeenNthCalledWith(2, "/api/v1/people/person-1", expect.objectContaining({ method: "PATCH", body: JSON.stringify({ name: "Alexander", notes: "Updated" }) }));
    expect(fetchMock).toHaveBeenNthCalledWith(3, "/api/v1/people/person-1/aliases", expect.objectContaining({ method: "POST", body: JSON.stringify({ alias: "Sasha" }) }));
    expect(fetchMock).toHaveBeenNthCalledWith(4, "/api/v1/people/person-1/aliases?alias=A%20%26%20S", expect.objectContaining({ method: "DELETE" }));
    expect(fetchMock).toHaveBeenNthCalledWith(5, "/api/v1/people/person-1/merge", expect.objectContaining({ method: "POST", body: JSON.stringify({ from: "person-2" }) }));
    expect(fetchMock).toHaveBeenNthCalledWith(6, "/api/v1/people/person-1", expect.objectContaining({ method: "DELETE" }));
  });

  it("manages correspondents and their aliases", async () => {
    const fetchMock = vi.fn().mockResolvedValue(response(200, {}));
    vi.stubGlobal("fetch", fetchMock);

    await api.createCorrespondent({ name: "North Bank", aliases: ["NB"] });
    await api.updateCorrespondent("corr-1", { name: "Northern Bank" });
    await api.addCorrespondentAlias("corr-1", "North");
    await api.removeCorrespondentAlias("corr-1", "NB");
    await api.mergeCorrespondents("corr-1", "corr-2");
    await api.deleteCorrespondent("corr-1");

    expect(fetchMock).toHaveBeenNthCalledWith(1, "/api/v1/correspondents", expect.objectContaining({ method: "POST", body: JSON.stringify({ name: "North Bank", aliases: ["NB"] }) }));
    expect(fetchMock).toHaveBeenNthCalledWith(2, "/api/v1/correspondents/corr-1", expect.objectContaining({ method: "PATCH", body: JSON.stringify({ name: "Northern Bank" }) }));
    expect(fetchMock).toHaveBeenNthCalledWith(3, "/api/v1/correspondents/corr-1/aliases", expect.objectContaining({ method: "POST", body: JSON.stringify({ alias: "North" }) }));
    expect(fetchMock).toHaveBeenNthCalledWith(4, "/api/v1/correspondents/corr-1/aliases?alias=NB", expect.objectContaining({ method: "DELETE" }));
    expect(fetchMock).toHaveBeenNthCalledWith(5, "/api/v1/correspondents/corr-1/merge", expect.objectContaining({ method: "POST", body: JSON.stringify({ from: "corr-2" }) }));
    expect(fetchMock).toHaveBeenNthCalledWith(6, "/api/v1/correspondents/corr-1", expect.objectContaining({ method: "DELETE" }));
  });

  it("creates, renames and deletes tags", async () => {
    const fetchMock = vi.fn()
      .mockResolvedValueOnce(response(201, {}))
      .mockResolvedValueOnce(response(200, {}))
      .mockResolvedValueOnce(new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", fetchMock);

    await api.createTag({ name: "warranty", color: "#63765a" });
    await api.renameTag("tag-1", "guarantee");
    await api.deleteTag("tag-1");

    expect(fetchMock).toHaveBeenNthCalledWith(1, "/api/v1/tags", expect.objectContaining({ method: "POST", body: JSON.stringify({ name: "warranty", color: "#63765a" }) }));
    expect(fetchMock).toHaveBeenNthCalledWith(2, "/api/v1/tags/tag-1", expect.objectContaining({ method: "PATCH", body: JSON.stringify({ name: "guarantee" }) }));
    expect(fetchMock).toHaveBeenNthCalledWith(3, "/api/v1/tags/tag-1", expect.objectContaining({ method: "DELETE" }));
  });

  it("manages trash, users, tokens and archive statistics", async () => {
    const fetchMock = vi.fn().mockResolvedValue(response(200, {}));
    vi.stubGlobal("fetch", fetchMock);

    await api.restoreDocument("doc-1");
    await api.purgeDocument("doc-1");
    await api.emptyTrash();
    await api.users();
    await api.createUser({ username: "sam", display_name: "Sam Example", password: "correct horse", locale: "ru", is_admin: false });
    await api.tokens();
    await api.createToken("Laptop");
    await api.revokeToken("token-1");
    await api.stats();

    expect(fetchMock).toHaveBeenNthCalledWith(1, "/api/v1/documents/doc-1/restore", expect.objectContaining({ method: "POST" }));
    expect(fetchMock).toHaveBeenNthCalledWith(2, "/api/v1/documents/doc-1", expect.objectContaining({ method: "DELETE" }));
    expect(fetchMock).toHaveBeenNthCalledWith(3, "/api/v1/trash", expect.objectContaining({ method: "DELETE" }));
    expect(fetchMock).toHaveBeenNthCalledWith(4, "/api/v1/users", expect.objectContaining({ credentials: "same-origin" }));
    expect(fetchMock).toHaveBeenNthCalledWith(5, "/api/v1/users", expect.objectContaining({ method: "POST", body: JSON.stringify({ username: "sam", display_name: "Sam Example", password: "correct horse", locale: "ru", is_admin: false }) }));
    expect(fetchMock).toHaveBeenNthCalledWith(6, "/api/v1/tokens", expect.objectContaining({ credentials: "same-origin" }));
    expect(fetchMock).toHaveBeenNthCalledWith(7, "/api/v1/tokens", expect.objectContaining({ method: "POST", body: JSON.stringify({ name: "Laptop" }) }));
    expect(fetchMock).toHaveBeenNthCalledWith(8, "/api/v1/tokens/token-1", expect.objectContaining({ method: "DELETE" }));
    expect(fetchMock).toHaveBeenNthCalledWith(9, "/api/v1/stats", expect.objectContaining({ credentials: "same-origin" }));
  });

  it("creates types and replaces their localized field definitions", async () => {
    const fetchMock = vi.fn().mockResolvedValue(response(200, {}));
    vi.stubGlobal("fetch", fetchMock);
    const fields = [{ key: "total", labels: { en: "Total", ru: "Сумма" }, kind: "money", required: true }] as const;

    await api.createType({ key: "receipt", labels: { en: "Receipt", ru: "Чек" }, fields: [...fields] });
    await api.updateType("type-1", { labels: { en: "Invoice", ru: "Счёт" }, fields: [...fields] });

    expect(fetchMock).toHaveBeenNthCalledWith(1, "/api/v1/types", expect.objectContaining({ method: "POST", body: JSON.stringify({ key: "receipt", labels: { en: "Receipt", ru: "Чек" }, fields }) }));
    expect(fetchMock).toHaveBeenNthCalledWith(2, "/api/v1/types/type-1", expect.objectContaining({ method: "PATCH", body: JSON.stringify({ labels: { en: "Invoice", ru: "Счёт" }, fields }) }));
  });
});
