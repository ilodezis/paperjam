import { afterAll, afterEach, beforeAll, describe, expect, it } from "vitest";
import { setupServer } from "msw/node";

import { handlers } from "./handlers";

const server = setupServer(...handlers);

beforeAll(() => server.listen());
afterEach(() => server.resetHandlers());
afterAll(() => server.close());

describe("document mock", () => {
  it("patches a title and returns the complete updated document", async () => {
    const response = await fetch(`${window.location.origin}/api/v1/documents/doc-passport`, {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ title: "Synthetic renewal" }),
    });

    expect(response.status).toBe(200);
    await expect(response.json()).resolves.toMatchObject({
      id: "doc-passport",
      title: "Synthetic renewal",
      title_provenance: "manual",
      file: { original_name: "doc-passport.pdf" },
    });
  });

  it("creates a short-lived download link", async () => {
    const response = await fetch(`${window.location.origin}/api/v1/documents/doc-rental/download-link`, { method: "POST" });

    expect(response.status).toBe(200);
    await expect(response.json()).resolves.toMatchObject({
      filename: "doc-rental.pdf",
      mime_type: "application/pdf",
    });
  });

  it("creates, lists and revokes share links", async () => {
    const created = await fetch(`${window.location.origin}/api/v1/documents/doc-rental/shares`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ ttl: "1h", show_details: false }),
    });
    expect(created.status).toBe(201);
    const share = (await created.json()) as { id: string; url: string; created_at: string; expires_at: string; show_details: boolean };
    expect(share.show_details).toBe(false);
    expect(Date.parse(share.expires_at) - Date.parse(share.created_at)).toBe(60 * 60_000);

    const listed = await fetch(`${window.location.origin}/api/v1/documents/doc-rental/shares`);
    await expect(listed.json()).resolves.toEqual([share]);

    const revoked = await fetch(`${window.location.origin}/api/v1/shares/${share.id}`, { method: "DELETE" });
    expect(revoked.status).toBe(204);
    const after = await fetch(`${window.location.origin}/api/v1/documents/doc-rental/shares`);
    await expect(after.json()).resolves.toEqual([]);
  });

  it("queues reprocessing and moves a document to trash", async () => {
    const reprocess = await fetch(`${window.location.origin}/api/v1/documents/doc-rental/reprocess`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ local: true, llm: false }),
    });
    expect(reprocess.status).toBe(202);
    await expect(reprocess.json()).resolves.toEqual({ id: "doc-rental", status: "queued" });

    const trash = await fetch(`${window.location.origin}/api/v1/documents/doc-rental/trash`, { method: "POST" });
    expect(trash.status).toBe(204);
  });
});

describe("taxonomy mock", () => {
  it("adds and removes an alias", async () => {
    const add = await fetch(`${window.location.origin}/api/v1/people/person-morgan/aliases`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ alias: "M. Sample" }),
    });
    expect(add.status).toBe(200);
    await expect(add.json()).resolves.toMatchObject({ aliases: ["M. Sample"] });

    const remove = await fetch(`${window.location.origin}/api/v1/people/person-morgan/aliases?alias=M.%20Sample`, { method: "DELETE" });
    expect(remove.status).toBe(200);
    await expect(remove.json()).resolves.toMatchObject({ aliases: [] });
  });

  it("merges a duplicate into the target and unites aliases and usage", async () => {
    const response = await fetch(`${window.location.origin}/api/v1/people/person-alex/merge`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ from: "person-morgan" }),
    });

    expect(response.status).toBe(200);
    await expect(response.json()).resolves.toMatchObject({
      id: "person-alex",
      aliases: ["A. Example", "Morgan Sample"],
      document_count: 5,
    });
    const listing = await fetch(`${window.location.origin}/api/v1/taxonomy`);
    const body = await listing.json() as { people: Array<{ id: string }> };
    expect(body.people.map((person) => person.id)).toEqual(["person-alex"]);
  });

  it("renames and deletes tags", async () => {
    const rename = await fetch(`${window.location.origin}/api/v1/tags/tag-renew`, {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name: "renewal" }),
    });
    expect(rename.status).toBe(200);
    await expect(rename.json()).resolves.toMatchObject({ id: "tag-renew", name: "renewal" });

    const remove = await fetch(`${window.location.origin}/api/v1/tags/tag-renew`, { method: "DELETE" });
    expect(remove.status).toBe(204);
  });

  it("replaces a document type's field definitions", async () => {
    const response = await fetch(`${window.location.origin}/api/v1/types/type-receipt`, {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ fields: [{ key: "warranty_until", labels: { en: "Warranty until", ru: "Гарантия до" }, kind: "date", required: false }] }),
    });

    expect(response.status).toBe(200);
    await expect(response.json()).resolves.toMatchObject({
      id: "type-receipt",
      labels: { en: "Receipt", ru: "Чек" },
      fields: [{ key: "warranty_until", kind: "date", required: false }],
    });
  });
});

describe("management mocks", () => {
  it("restores and permanently removes trashed documents", async () => {
    await fetch(`${window.location.origin}/api/v1/documents/doc-insurance/trash`, { method: "POST" });
    const restore = await fetch(`${window.location.origin}/api/v1/documents/doc-insurance/restore`, { method: "POST" });
    expect(restore.status).toBe(204);
    const restored = await fetch(`${window.location.origin}/api/v1/documents/doc-insurance`);
    await expect(restored.json()).resolves.toMatchObject({ trashed_at: null });

    await fetch(`${window.location.origin}/api/v1/documents/doc-insurance/trash`, { method: "POST" });
    const purge = await fetch(`${window.location.origin}/api/v1/documents/doc-insurance`, { method: "DELETE" });
    expect(purge.status).toBe(204);
    const empty = await fetch(`${window.location.origin}/api/v1/trash`, { method: "DELETE" });
    expect(empty.status).toBe(200);
    await expect(empty.json()).resolves.toEqual({ purged: 1 });
  });

  it("creates users and reveals a new token secret only once", async () => {
    const user = await fetch(`${window.location.origin}/api/v1/users`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ username: "sam", display_name: "Sam Example", password: "correct horse", locale: "ru", is_admin: false }),
    });
    expect(user.status).toBe(201);
    await expect(user.json()).resolves.toMatchObject({ username: "sam", display_name: "Sam Example", is_admin: false });

    const created = await fetch(`${window.location.origin}/api/v1/tokens`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name: "Laptop" }),
    });
    expect(created.status).toBe(201);
    const body = await created.json() as { token: { id: string; prefix: string }; secret: string };
    expect(body.secret).toMatch(/^pj_/);
    expect(body.token.prefix).not.toBe(body.secret);

    const listing = await fetch(`${window.location.origin}/api/v1/tokens`);
    const listed = await listing.json() as Array<Record<string, unknown>>;
    expect(listed.length).toBeGreaterThan(0);
    expect(listed.every((token) => !("secret" in token))).toBe(true);
    const revoke = await fetch(`${window.location.origin}/api/v1/tokens/${body.token.id}`, { method: "DELETE" });
    expect(revoke.status).toBe(204);

    const stats = await fetch(`${window.location.origin}/api/v1/stats`);
    await expect(stats.json()).resolves.toMatchObject({ queue: { queued: 0, running: 0, dead: 0 }, llm_month_usd: "0.0421" });
  });
});
