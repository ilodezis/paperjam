import { http, HttpResponse } from "msw";

import type { Connection, DocumentEvent, DocumentOrganization, DocumentPatch, DocumentView, ManagedUser, PasskeyView, Share, ShareTtl, TokenView } from "../api";
import { attentionDocuments, documentTags, documentViews, expiringDocuments, hits, mockUser, taxonomy } from "./data";

let signedIn = true;
let uploadCounter = 0;
const uploadPolls = new Map<string, number>();
const users: ManagedUser[] = [{ ...mockUser, person_id: "person-alex", created_at: "2026-01-01T00:00:00Z", disabled_at: null }];
const passkeys: PasskeyView[] = [{ id: "passkey-phone", credential_id: "cGhvbmU", label: "Demo phone", created_at: "2026-02-03T09:00:00Z", last_used_at: "2026-02-14T07:45:00Z" }];
let ceremonies = 0;
let calendarFeed: { created_at: string; last_used_at: string | null } | null = null;
const connections: Connection[] = [{ id: "grant-assistant", client_id: "client-assistant", client_name: "Assistant", created_at: "2026-02-03T10:00:00Z", last_used_at: "2026-02-12T07:45:00Z" }];
const tokens: TokenView[] = [{ id: "token-reader", user_id: mockUser.id, name: "Reading device", prefix: "pj_demo_read", created_at: "2026-02-01T09:00:00Z", last_used_at: "2026-02-12T08:30:00Z", revoked_at: null }];

interface SearchBody {
  query?: string | null;
  sort?: "relevance" | "date_desc" | "date_asc";
  cursor?: string | null;
  filters?: { include_trashed?: boolean };
}

function escapeHtml(value: string): string {
  return value.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;");
}

/** Share links created in this session, by document. */
const mockShares: Record<string, Share[]> = {};

export const handlers = [
  http.get("/api/v1/setup", () => HttpResponse.json({ needed: false })),
  http.post("/api/v1/setup", async ({ request }) => {
    const body = await request.json() as { code?: string; username?: string };
    if (body.code !== "aaaa-bbbb-cccc") {
      return HttpResponse.json({ code: "invalid_setup_code", message: "Wrong code" }, { status: 422 });
    }
    signedIn = true;
    return HttpResponse.json({ user: { ...mockUser, username: body.username ?? "demo", is_admin: true } });
  }),
  http.get("/api/v1/auth/session", () => {
    if (!signedIn) return HttpResponse.json({ code: "unauthorized", message: "Sign in required" }, { status: 401 });
    return HttpResponse.json({ user: mockUser, via: "cookie" });
  }),
  http.patch("/api/v1/auth/session", async ({ request }) => {
    if (!signedIn) return HttpResponse.json({ code: "unauthorized", message: "Sign in required" }, { status: 401 });
    const body = await request.json() as { locale: string };
    return HttpResponse.json({ user: { ...mockUser, locale: body.locale }, via: "cookie" });
  }),
  http.post("/api/v1/auth/login", async ({ request }) => {
    const body = await request.json() as { username?: string; password?: string };
    if (!body.username || !body.password) {
      return HttpResponse.json({ code: "invalid_credentials", message: "Invalid credentials" }, { status: 401 });
    }
    signedIn = true;
    return HttpResponse.json({ user: { ...mockUser, username: body.username, display_name: body.username } });
  }),
  http.post("/api/v1/auth/passkey/challenge", async ({ request }) => {
    const body = await request.json() as { username?: string };
    const allowCredentials = body.username ? passkeys.map((p) => ({ type: "public-key", id: p.credential_id })) : [];
    ceremonies += 1;
    return HttpResponse.json({ ceremony: `ceremony-login-${ceremonies}`, options: { challenge: "Y2hhbGxlbmdl", timeout: 300000, rpId: "localhost", allowCredentials, userVerification: "required" } });
  }),
  http.post("/api/v1/auth/passkey", async ({ request }) => {
    const body = await request.json() as { ceremony: string; credential: { id: string } };
    const passkey = passkeys.find((p) => p.credential_id === body.credential.id);
    if (!passkey) return HttpResponse.json({ code: "unknown_passkey", message: "Unknown passkey" }, { status: 401 });
    passkey.last_used_at = new Date().toISOString();
    signedIn = true;
    return HttpResponse.json({ user: mockUser });
  }),
  http.post("/api/v1/auth/logout", () => {
    signedIn = false;
    return new HttpResponse(null, { status: 204 });
  }),
  http.post("/api/v1/auth/password", async ({ request }) => {
    if (!signedIn) return HttpResponse.json({ code: "unauthorized", message: "Sign in required" }, { status: 401 });
    const body = await request.json() as { current_password?: string; new_password?: string };
    if (body.current_password === "wrong password") {
      return HttpResponse.json({ code: "wrong_password", message: "The current password is incorrect" }, { status: 403 });
    }
    if (!body.new_password || body.new_password.length < 8) {
      return HttpResponse.json({ code: "weak_password", message: "Password too short" }, { status: 422 });
    }
    return new HttpResponse(null, { status: 204 });
  }),
  http.get("/api/v1/taxonomy", () => HttpResponse.json(taxonomy)),
  http.post("/api/v1/people", async ({ request }) => {
    const body = await request.json() as { name: string; aliases?: string[]; notes?: string };
    const person = { id: `person-${crypto.randomUUID()}`, name: body.name.trim(), aliases: body.aliases ?? [], notes: body.notes ?? "", created_at: new Date().toISOString(), document_count: 0 };
    taxonomy.people.push(person);
    return HttpResponse.json(person, { status: 201 });
  }),
  http.patch("/api/v1/people/:id", async ({ params, request }) => {
    const person = taxonomy.people.find((item) => item.id === params.id);
    if (!person) return HttpResponse.json({ code: "not_found", message: "Person not found" }, { status: 404 });
    const body = await request.json() as { name?: string; notes?: string };
    if (body.name !== undefined) person.name = body.name.trim();
    if (body.notes !== undefined) person.notes = body.notes.trim();
    return HttpResponse.json(person);
  }),
  http.post("/api/v1/people/:id/aliases", async ({ params, request }) => {
    const person = taxonomy.people.find((item) => item.id === params.id);
    if (!person) return HttpResponse.json({ code: "not_found", message: "Person not found" }, { status: 404 });
    const body = await request.json() as { alias: string };
    if (!person.aliases.includes(body.alias)) person.aliases.push(body.alias);
    return HttpResponse.json(person);
  }),
  http.delete("/api/v1/people/:id/aliases", ({ params, request }) => {
    const person = taxonomy.people.find((item) => item.id === params.id);
    if (!person) return HttpResponse.json({ code: "not_found", message: "Person not found" }, { status: 404 });
    const alias = new URL(request.url).searchParams.get("alias");
    person.aliases = person.aliases.filter((item) => item !== alias);
    return HttpResponse.json(person);
  }),
  http.post("/api/v1/people/:id/merge", async ({ params, request }) => {
    const target = taxonomy.people.find((item) => item.id === params.id);
    const body = await request.json() as { from: string };
    const sourceIndex = taxonomy.people.findIndex((item) => item.id === body.from);
    if (!target || sourceIndex < 0) return HttpResponse.json({ code: "not_found", message: "Person not found" }, { status: 404 });
    const [source] = taxonomy.people.splice(sourceIndex, 1);
    target.aliases = [...new Set([...target.aliases, ...source.aliases, source.name])];
    target.document_count += source.document_count;
    return HttpResponse.json(target);
  }),
  http.post("/api/v1/people/:id/relink", ({ params }) => {
    const person = taxonomy.people.find((item) => item.id === params.id);
    if (!person) return HttpResponse.json({ code: "not_found", message: "Person not found" }, { status: 404 });
    return HttpResponse.json(person, { status: 202 });
  }),
  http.delete("/api/v1/people/:id", ({ params }) => {
    const index = taxonomy.people.findIndex((item) => item.id === params.id);
    if (index < 0) return HttpResponse.json({ code: "not_found", message: "Person not found" }, { status: 404 });
    taxonomy.people.splice(index, 1);
    return new HttpResponse(null, { status: 204 });
  }),
  http.post("/api/v1/correspondents", async ({ request }) => {
    const body = await request.json() as { name: string; aliases?: string[] };
    const correspondent = { id: `corr-${crypto.randomUUID()}`, name: body.name.trim(), aliases: body.aliases ?? [], created_at: new Date().toISOString(), document_count: 0 };
    taxonomy.correspondents.push(correspondent);
    return HttpResponse.json(correspondent, { status: 201 });
  }),
  http.patch("/api/v1/correspondents/:id", async ({ params, request }) => {
    const correspondent = taxonomy.correspondents.find((item) => item.id === params.id);
    if (!correspondent) return HttpResponse.json({ code: "not_found", message: "Correspondent not found" }, { status: 404 });
    const body = await request.json() as { name?: string };
    if (body.name !== undefined) correspondent.name = body.name.trim();
    return HttpResponse.json(correspondent);
  }),
  http.post("/api/v1/correspondents/:id/aliases", async ({ params, request }) => {
    const correspondent = taxonomy.correspondents.find((item) => item.id === params.id);
    if (!correspondent) return HttpResponse.json({ code: "not_found", message: "Correspondent not found" }, { status: 404 });
    const body = await request.json() as { alias: string };
    if (!correspondent.aliases.includes(body.alias)) correspondent.aliases.push(body.alias);
    return HttpResponse.json(correspondent);
  }),
  http.delete("/api/v1/correspondents/:id/aliases", ({ params, request }) => {
    const correspondent = taxonomy.correspondents.find((item) => item.id === params.id);
    if (!correspondent) return HttpResponse.json({ code: "not_found", message: "Correspondent not found" }, { status: 404 });
    const alias = new URL(request.url).searchParams.get("alias");
    correspondent.aliases = correspondent.aliases.filter((item) => item !== alias);
    return HttpResponse.json(correspondent);
  }),
  http.post("/api/v1/correspondents/:id/merge", async ({ params, request }) => {
    const target = taxonomy.correspondents.find((item) => item.id === params.id);
    const body = await request.json() as { from: string };
    const sourceIndex = taxonomy.correspondents.findIndex((item) => item.id === body.from);
    if (!target || sourceIndex < 0) return HttpResponse.json({ code: "not_found", message: "Correspondent not found" }, { status: 404 });
    const [source] = taxonomy.correspondents.splice(sourceIndex, 1);
    target.aliases = [...new Set([...target.aliases, ...source.aliases, source.name])];
    target.document_count += source.document_count;
    return HttpResponse.json(target);
  }),
  http.delete("/api/v1/correspondents/:id", ({ params }) => {
    const index = taxonomy.correspondents.findIndex((item) => item.id === params.id);
    if (index < 0) return HttpResponse.json({ code: "not_found", message: "Correspondent not found" }, { status: 404 });
    taxonomy.correspondents.splice(index, 1);
    return new HttpResponse(null, { status: 204 });
  }),
  http.post("/api/v1/tags", async ({ request }) => {
    const body = await request.json() as { name: string; color?: string | null };
    const tag = { id: `tag-${crypto.randomUUID()}`, name: body.name.trim(), color: body.color ?? null, document_count: 0 };
    taxonomy.tags.push(tag);
    return HttpResponse.json(tag, { status: 201 });
  }),
  http.patch("/api/v1/tags/:id", async ({ params, request }) => {
    const tag = taxonomy.tags.find((item) => item.id === params.id);
    if (!tag) return HttpResponse.json({ code: "not_found", message: "Tag not found" }, { status: 404 });
    const body = await request.json() as { name: string };
    tag.name = body.name.trim();
    return HttpResponse.json(tag);
  }),
  http.delete("/api/v1/tags/:id", ({ params }) => {
    const index = taxonomy.tags.findIndex((item) => item.id === params.id);
    if (index < 0) return HttpResponse.json({ code: "not_found", message: "Tag not found" }, { status: 404 });
    taxonomy.tags.splice(index, 1);
    return new HttpResponse(null, { status: 204 });
  }),
  http.post("/api/v1/types", async ({ request }) => {
    const body = await request.json() as { key: string; labels: Record<string, string>; fields: import("../api").FieldDef[] };
    const type = { id: `type-${crypto.randomUUID()}`, ...body, builtin: false, created_at: new Date().toISOString(), document_count: 0 };
    taxonomy.types.push(type);
    return HttpResponse.json(type, { status: 201 });
  }),
  http.patch("/api/v1/types/:id", async ({ params, request }) => {
    const type = taxonomy.types.find((item) => item.id === params.id);
    if (!type) return HttpResponse.json({ code: "not_found", message: "Document type not found" }, { status: 404 });
    const body = await request.json() as { labels?: Record<string, string>; fields?: import("../api").FieldDef[] };
    if (body.labels) type.labels = body.labels;
    if (body.fields) type.fields = body.fields;
    return HttpResponse.json(type);
  }),
  http.post("/api/v1/search", async ({ request }) => {
    const body = await request.json() as SearchBody;
    const query = body.query ?? "";
    const type = query.match(/(?:^|\s)type:([^\s]+)/)?.[1];
    const person = query.match(/(?:^|\s)person:"?([^"\s]+)/)?.[1];
    const from = query.match(/(?:^|\s)from:"?([^"\s]+)/)?.[1];
    const tag = query.match(/(?:^|\s)tag:([^\s]+)/)?.[1];
    const free = query.replace(/(?:type|person|from|tag|has):(?:"[^"]+"|\S+)/g, "").trim().toLowerCase();
    let result = hits.filter((hit) => {
      if (!body.filters?.include_trashed && documentViews[hit.id]?.trashed_at) return false;
      if (type && hit.type_key !== type) return false;
      if (person && !hit.people.some((name) => name.toLowerCase().includes(person.toLowerCase()))) return false;
      if (from && !hit.organizations.some((name) => name.toLowerCase().includes(from.toLowerCase()))) return false;
      if (tag && !(documentTags[hit.id] ?? []).includes(tag)) return false;
      if (query.includes("has:attention") && !attentionDocuments.has(hit.id)) return false;
      if (query.includes("has:expiring") && !expiringDocuments.has(hit.id)) return false;
      if (free && !`${hit.title} ${hit.snippet} ${hit.organizations.join(" ")} ${hit.people.join(" ")}`.toLowerCase().includes(free)) return false;
      return true;
    });
    if (body.sort === "date_desc") result = [...result].sort((a, b) => (b.document_date ?? "").localeCompare(a.document_date ?? ""));
    if (body.sort === "date_asc") result = [...result].sort((a, b) => (a.document_date ?? "").localeCompare(b.document_date ?? ""));
    const offset = Number(body.cursor ?? 0);
    const page = result.slice(offset, offset + 20);
    const term = free.split(/\s+/)[0];
    return HttpResponse.json({
      hits: page.map((hit) => ({
        ...hit,
        snippet: term && hit.snippet.toLowerCase().includes(term)
          ? escapeHtml(hit.snippet).replace(new RegExp(`(${term})`, "i"), "<mark>$1</mark>")
          : hit.snippet,
      })),
      total: result.length,
      next_cursor: offset + 20 < result.length ? String(offset + 20) : null,
    });
  }),
  http.post("/api/v1/documents", async ({ request }) => {
    const form = await request.formData();
    const file = form.get("file");
    const name = file instanceof File ? file.name : "document.pdf";
    if (name.toLowerCase().includes("duplicate")) {
      return HttpResponse.json({ code: "duplicate", message: "Already archived", document_id: "doc-receipt" }, { status: 409 });
    }
    const id = `doc-upload-${++uploadCounter}`;
    uploadPolls.set(id, name.toLowerCase().includes("failed") ? -1 : 0);
    const template = documentViews["doc-receipt"]!;
    const uploaded: DocumentView = {
      ...template,
      id,
      title: name.replace(/\.[^.]+$/, ""),
      title_provenance: "local",
      local_only: form.get("local_only") === "true",
      status: "queued",
      status_reason: null,
      attention: [],
      fields: [],
      facts: [],
      file: { ...template.file, original_name: name, size: file instanceof File ? file.size : 0, page_count: 1 },
      text: null,
    };
    documentViews[id] = uploaded;
    return HttpResponse.json({ id, status: "queued", restored: name.toLowerCase().includes("restored") }, { status: 201 });
  }),
  http.get("/api/v1/documents/:id", ({ params, request }) => {
    const id = String(params.id);
    const document = documentViews[id];
    if (!document) return HttpResponse.json({ code: "not_found", message: "Document not found" }, { status: 404 });
    if (uploadPolls.has(id)) {
      const polls = uploadPolls.get(id) ?? 0;
      uploadPolls.set(id, polls + 1);
      document.status = polls < 0 ? "failed" : polls === 0 ? "processing" : "ready";
      document.status_reason = polls < 0 ? "Synthetic processing failure for interface testing." : null;
    }
    const textPage = Number(new URL(request.url).searchParams.get("text_page") ?? 1);
    const text = document.text ? {
      ...document.text,
      page: textPage,
      text: textPage === 2 ? "Second synthetic passport page.\nEmergency contact and renewal notes.\nNo real personal data." : document.text.text,
    } : null;
    return HttpResponse.json({ ...document, text });
  }),
  http.get("/api/v1/documents/:id/events", ({ params }) => {
    const id = String(params.id);
    const document = documentViews[id];
    if (!document) return HttpResponse.json({ code: "not_found", message: "Document not found" }, { status: 404 });
    const events: DocumentEvent[] = [
      { id: 3, action: "document.update", via: "web", at: document.updated_at, actor_name: mockUser.display_name, diff: { title: document.title } },
      { id: 2, action: "document.process", via: "system", at: document.created_at, actor_name: null, diff: {} },
      {
        id: 1,
        action: "document.upload",
        via: id === "doc-receipt" ? "email" : "web",
        at: document.created_at,
        actor_name: id === "doc-receipt" ? null : mockUser.display_name,
        diff: {},
      },
    ];
    return HttpResponse.json(events);
  }),
  http.patch("/api/v1/documents/:id", async ({ params, request }) => {
    const id = String(params.id);
    const current = documentViews[id];
    if (!current) return HttpResponse.json({ code: "not_found", message: "Document not found" }, { status: 404 });
    const patch = await request.json() as DocumentPatch;
    const updated: DocumentView = { ...current, updated_at: new Date().toISOString(), updated_by: mockUser.id };
    if ("title" in patch) {
      updated.title = patch.title ?? null;
      updated.title_provenance = "manual";
    }
    if ("document_date" in patch) {
      updated.document_date = patch.document_date ?? null;
      updated.date_provenance = "manual";
    }
    if ("type_id" in patch) {
      updated.type_id = patch.type_id ?? null;
      updated.type_key = taxonomy.types.find((type) => type.id === patch.type_id)?.key ?? null;
      updated.type_provenance = "manual";
    }
    if ("organizations" in patch && patch.organizations !== undefined) {
      for (const org of patch.organizations) {
        if (!taxonomy.correspondents.some((item) => item.id === org.id)) {
          return HttpResponse.json({ code: "correspondent_not_found", message: "Correspondent not found" }, { status: 404 });
        }
      }
      const ROLE_ORDER: Record<string, number> = {
        issued_by: 0,
        received_from: 1,
        party: 2,
        employer: 3,
        unspecified: 4,
      };
      const seen = new Set<string>();
      const list: DocumentOrganization[] = [];
      for (const item of patch.organizations) {
        const role = item.role ?? "unspecified";
        const key = `${item.id}:${role}`;
        if (seen.has(key)) continue;
        seen.add(key);
        const corr = taxonomy.correspondents.find((c) => c.id === item.id)!;
        list.push({
          id: corr.id,
          name: corr.name,
          role,
          provenance: "manual",
        });
      }
      list.sort((a, b) => {
        const orderDiff = (ROLE_ORDER[a.role] ?? 4) - (ROLE_ORDER[b.role] ?? 4);
        if (orderDiff !== 0) return orderDiff;
        return a.name.localeCompare(b.name);
      });
      updated.organizations = list;
    }
    if (patch.people) updated.people = patch.people.map((personId) => {
      const person = taxonomy.people.find((item) => item.id === personId)!;
      return { id: person.id, name: person.name, provenance: "manual" };
    });
    if (patch.tags) {
      updated.tags = patch.tags.map((tagId) => {
        const tag = taxonomy.tags.find((item) => item.id === tagId)!;
        return { id: tag.id, name: tag.name, provenance: "manual" };
      });
      documentTags[id] = updated.tags.map((tag) => tag.name);
    }
    if (patch.fields) {
      const fields = current.fields.filter((entry) => !(entry.key in patch.fields!));
      for (const [key, value] of Object.entries(patch.fields)) {
        if (value) fields.push({ key, value, provenance: "manual", page: 1 });
      }
      updated.fields = fields;
    }
    if (patch.dismiss_attention && patch.dismiss_attention.length > 0) {
      const drop = new Set(patch.dismiss_attention);
      updated.attention = current.attention.filter((flag) => !drop.has(flag));
      updated.fields = updated.fields.map((entry) => (
        entry.issues ? { ...entry, issues: entry.issues.filter((flag) => !drop.has(flag)) } : entry
      ));
    }
    if ("summary" in patch) {
      updated.summary = patch.summary ?? null;
      updated.summary_provenance = "manual";
    }
    if ("local_only" in patch) updated.local_only = patch.local_only ?? false;
    documentViews[id] = updated;
    const hit = hits.find((item) => item.id === id);
    if (hit) {
      hit.title = updated.title ?? updated.file.original_name;
      hit.document_date = updated.document_date;
      hit.type_key = updated.type_key;
      hit.organizations = updated.organizations.map((org) => org.name);
      hit.people = updated.people.map((person) => person.name);
      hit.key_fields = Object.fromEntries(updated.fields.map((field) => [field.key, field.value]));
      hit.attention = updated.attention;
    }
    return HttpResponse.json(updated);
  }),
  http.post("/api/v1/documents/:id/download-link", ({ params, request }) => {
    const document = documentViews[String(params.id)];
    if (!document) return HttpResponse.json({ code: "not_found", message: "Document not found" }, { status: 404 });
    return HttpResponse.json({
      url: new URL(`/api/v1/dl/${document.id}-synthetic`, request.url).toString(),
      expires_at: new Date(Date.now() + 10 * 60_000).toISOString(),
      filename: document.file.original_name,
      mime_type: document.file.mime,
      size: document.file.size,
    });
  }),
  http.get("/api/v1/documents/:id/shares", ({ params }) => {
    const id = String(params.id);
    const now = Date.now();
    return HttpResponse.json((mockShares[id] ?? []).filter((share) => Date.parse(share.expires_at) > now));
  }),
  http.post("/api/v1/documents/:id/shares", async ({ params, request }) => {
    const document = documentViews[String(params.id)];
    if (!document) return HttpResponse.json({ code: "document_not_found", message: "no such document" }, { status: 404 });
    const input = (await request.json().catch(() => ({}))) as { ttl?: ShareTtl; show_details?: boolean };
    const minutes = { "15m": 15, "1h": 60, "24h": 24 * 60 }[input.ttl ?? "15m"] ?? 15;
    const id = `share-${Object.values(mockShares).flat().length + 1}`;
    const created = new Date();
    const share: Share = {
      id,
      url: new URL(`/s/${id}synthetic`, request.url).toString(),
      created_at: created.toISOString(),
      expires_at: new Date(created.getTime() + minutes * 60_000).toISOString(),
      show_details: input.show_details ?? true,
    };
    mockShares[document.id] = [share, ...(mockShares[document.id] ?? [])];
    return HttpResponse.json(share, { status: 201 });
  }),
  http.delete("/api/v1/shares/:id", ({ params }) => {
    const id = String(params.id);
    const owner = Object.keys(mockShares).find((doc) => mockShares[doc]?.some((share) => share.id === id));
    if (!owner) return HttpResponse.json({ code: "share_not_found", message: "no such share link" }, { status: 404 });
    mockShares[owner] = (mockShares[owner] ?? []).filter((share) => share.id !== id);
    return new HttpResponse(null, { status: 204 });
  }),
  http.post("/api/v1/documents/:id/reprocess", ({ params }) => {
    const id = String(params.id);
    const document = documentViews[id];
    if (!document) return HttpResponse.json({ code: "not_found", message: "Document not found" }, { status: 404 });
    document.status = "queued";
    document.status_reason = null;
    uploadPolls.set(id, 0);
    return HttpResponse.json({ id, status: "queued" }, { status: 202 });
  }),
  http.post("/api/v1/documents/:id/trash", ({ params }) => {
    const document = documentViews[String(params.id)];
    if (!document) return HttpResponse.json({ code: "not_found", message: "Document not found" }, { status: 404 });
    document.trashed_at = new Date().toISOString();
    return new HttpResponse(null, { status: 204 });
  }),
  http.post("/api/v1/documents/:id/restore", ({ params }) => {
    const document = documentViews[String(params.id)];
    if (!document) return HttpResponse.json({ code: "not_found", message: "Document not found" }, { status: 404 });
    document.trashed_at = null;
    return new HttpResponse(null, { status: 204 });
  }),
  http.delete("/api/v1/documents/:id", ({ params }) => {
    const id = String(params.id);
    if (!documentViews[id]?.trashed_at) return HttpResponse.json({ code: "not_found", message: "Trashed document not found" }, { status: 404 });
    delete documentViews[id];
    const index = hits.findIndex((hit) => hit.id === id);
    if (index >= 0) hits.splice(index, 1);
    return new HttpResponse(null, { status: 204 });
  }),
  http.delete("/api/v1/trash", () => {
    let purged = 0;
    for (const [id, document] of Object.entries(documentViews)) {
      if (!document.trashed_at) continue;
      delete documentViews[id];
      const index = hits.findIndex((hit) => hit.id === id);
      if (index >= 0) hits.splice(index, 1);
      purged += 1;
    }
    return HttpResponse.json({ purged });
  }),
  http.get("/api/v1/users", () => HttpResponse.json(users)),
  http.post("/api/v1/users", async ({ request }) => {
    const body = await request.json() as { username: string; display_name?: string; locale: string; is_admin: boolean };
    const user: ManagedUser = { id: `user-${crypto.randomUUID()}`, username: body.username, display_name: body.display_name ?? body.username, locale: body.locale, is_admin: body.is_admin, person_id: null, created_at: new Date().toISOString(), disabled_at: null };
    users.push(user);
    return HttpResponse.json(user, { status: 201 });
  }),
  http.patch("/api/v1/users/:id", async ({ params, request }) => {
    const user = users.find((item) => item.id === params.id);
    if (!user) return HttpResponse.json({ code: "user_not_found", message: "No such user" }, { status: 404 });
    const body = await request.json() as { disabled?: boolean; is_admin?: boolean; display_name?: string; password?: string };
    if (body.disabled === true && user.id === mockUser.id) {
      return HttpResponse.json({ code: "cannot_disable_self", message: "Cannot disable self" }, { status: 422 });
    }
    if (body.disabled !== undefined) {
      user.disabled_at = body.disabled ? new Date().toISOString() : null;
    }
    if (body.is_admin !== undefined) user.is_admin = body.is_admin;
    if (body.display_name !== undefined) {
      const trimmed = body.display_name.trim();
      if (!trimmed) return HttpResponse.json({ code: "invalid_display_name", message: "Display name cannot be empty" }, { status: 422 });
      user.display_name = trimmed;
    }
    return HttpResponse.json(user);
  }),
  http.delete("/api/v1/users/:id", ({ params }) => {
    const index = users.findIndex((item) => item.id === params.id);
    if (index < 0) return HttpResponse.json({ code: "user_not_found", message: "No such user" }, { status: 404 });
    if (users[index].id === mockUser.id) {
      return HttpResponse.json({ code: "cannot_delete_self", message: "Cannot delete self" }, { status: 422 });
    }
    users.splice(index, 1);
    return new HttpResponse(null, { status: 204 });
  }),
  http.get("/api/v1/calendar-feed", () => HttpResponse.json({ active: calendarFeed !== null, feed: calendarFeed, lead_days: 60 })),
  http.post("/api/v1/calendar-feed", () => {
    calendarFeed = { created_at: new Date().toISOString(), last_used_at: null };
    const secret = `pjc_${crypto.randomUUID().replaceAll("-", "")}`;
    return HttpResponse.json({ feed: calendarFeed, secret, path: `/calendar/${secret}.ics`, url: null }, { status: 201 });
  }),
  http.delete("/api/v1/calendar-feed", () => {
    if (!calendarFeed) return HttpResponse.json({ code: "calendar_feed_not_found", message: "No feed" }, { status: 404 });
    calendarFeed = null;
    return new HttpResponse(null, { status: 204 });
  }),
  http.get("/api/v1/tokens", () => HttpResponse.json(tokens)),
  http.post("/api/v1/tokens", async ({ request }) => {
    const body = await request.json() as { name: string };
    const suffix = crypto.randomUUID().replaceAll("-", "");
    const token: TokenView = { id: `token-${crypto.randomUUID()}`, user_id: mockUser.id, name: body.name, prefix: `pj_demo_${suffix.slice(0, 8)}`, created_at: new Date().toISOString(), last_used_at: null, revoked_at: null };
    tokens.push(token);
    return HttpResponse.json({ token, secret: `${token.prefix}_${suffix}` }, { status: 201 });
  }),
  http.delete("/api/v1/tokens/:id", ({ params }) => {
    const token = tokens.find((item) => item.id === params.id);
    if (!token) return HttpResponse.json({ code: "not_found", message: "Token not found" }, { status: 404 });
    token.revoked_at = new Date().toISOString();
    return new HttpResponse(null, { status: 204 });
  }),
  http.get("/api/v1/connections", () => HttpResponse.json(connections)),
  http.delete("/api/v1/connections/:id", ({ params }) => {
    const index = connections.findIndex((item) => item.id === params.id);
    if (index < 0) return HttpResponse.json({ code: "grant_not_found", message: "no such grant" }, { status: 404 });
    connections.splice(index, 1);
    return new HttpResponse(null, { status: 204 });
  }),
  http.get("/api/v1/passkeys", () => HttpResponse.json({ available: true, rp_id: "localhost", user_handle: "AYAAAAAAcACAAAAAAAAAAQ", passkeys })),
  http.post("/api/v1/passkeys/challenge", () => {
    ceremonies += 1;
    return HttpResponse.json({
      ceremony: `ceremony-register-${ceremonies}`,
      options: {
        rp: { id: "localhost", name: "paperjam" },
        user: { id: "AYAAAAAAcACAAAAAAAAAAQ", name: mockUser.username, displayName: mockUser.display_name },
        challenge: "Y2hhbGxlbmdl",
        pubKeyCredParams: [{ type: "public-key", alg: -7 }],
        timeout: 300000,
        excludeCredentials: passkeys.map((p) => ({ type: "public-key", id: p.credential_id })),
        authenticatorSelection: { residentKey: "preferred", requireResidentKey: false, userVerification: "required" },
        attestation: "none",
        hints: null,
      },
    });
  }),
  http.post("/api/v1/passkeys", async ({ request }) => {
    const body = await request.json() as { ceremony: string; label: string; credential: { id: string } };
    if (passkeys.some((p) => p.credential_id === body.credential.id)) {
      return HttpResponse.json({ code: "passkey_already_registered", message: "Already registered" }, { status: 409 });
    }
    const passkey: PasskeyView = { id: `passkey-${crypto.randomUUID()}`, credential_id: body.credential.id, label: body.label, created_at: new Date().toISOString(), last_used_at: null };
    passkeys.push(passkey);
    return HttpResponse.json(passkey, { status: 201 });
  }),
  http.delete("/api/v1/passkeys/:id", ({ params }) => {
    const index = passkeys.findIndex((p) => p.id === params.id);
    if (index < 0) return HttpResponse.json({ code: "passkey_not_found", message: "No such passkey" }, { status: 404 });
    passkeys.splice(index, 1);
    return new HttpResponse(null, { status: 204 });
  }),
  http.get("/api/v1/stats", () => {
    const active = Object.values(documentViews).filter((document) => !document.trashed_at);
    return HttpResponse.json({
      documents: active.length,
      by_status: Object.fromEntries(["queued", "processing", "ready", "failed"].map((status) => [status, active.filter((document) => document.status === status).length])),
      attention: active.filter((document) => document.attention.length > 0).length,
      queue: { queued: 0, running: 0, dead: 0 },
      llm_month_usd: "0.0421",
    });
  }),
  http.get("/api/v1/documents/:id/original", ({ params }) => {
    const document = documentViews[String(params.id)];
    if (!document) return HttpResponse.json({ code: "not_found", message: "Document not found" }, { status: 404 });
    return HttpResponse.text("%PDF-1.4\n% Synthetic paperjam fixture\n", { headers: { "Content-Type": "application/pdf", "Content-Disposition": `attachment; filename="${document.file.original_name}"` } });
  }),
  http.get("/api/v1/documents/:id/pages/:page.webp", ({ params }) => {
    const document = documentViews[String(params.id)];
    if (!document) return HttpResponse.json({ code: "not_found", message: "Document not found" }, { status: 404 });
    const page = String(params.page);
    const title = escapeHtml(document.title ?? document.file.original_name);
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="800" height="1040" viewBox="0 0 800 1040"><rect width="800" height="1040" fill="#f9f7f0"/><rect x="64" y="62" width="672" height="7" fill="#63765a"/><text x="64" y="126" font-family="sans-serif" font-size="30" fill="#2d332e">${title}</text><text x="64" y="170" font-family="sans-serif" font-size="16" fill="#73786f">SYNTHETIC ARCHIVE FIXTURE · PAGE ${page}</text><rect x="64" y="228" width="570" height="9" fill="#b7b8af"/><rect x="64" y="260" width="650" height="6" fill="#d4d3ca"/><rect x="64" y="282" width="610" height="6" fill="#d4d3ca"/><rect x="64" y="304" width="640" height="6" fill="#d4d3ca"/><rect x="64" y="378" width="280" height="160" fill="#e8e5dc"/><rect x="64" y="610" width="650" height="6" fill="#d4d3ca"/><rect x="64" y="632" width="590" height="6" fill="#d4d3ca"/><rect x="64" y="654" width="630" height="6" fill="#d4d3ca"/><text x="64" y="948" font-family="sans-serif" font-size="14" fill="#8a8d84">${page} / ${document.file.page_count ?? 1}</text></svg>`;
    return HttpResponse.text(svg, { headers: { "Content-Type": "image/svg+xml", "Cache-Control": "max-age=3600" } });
  }),
  http.get("/api/v1/documents/:id/thumbnail.webp", ({ params }) => {
    const colors: Record<string, string> = { "doc-passport": "#667b72", "doc-rental": "#9a8060", "doc-receipt": "#798477", "doc-insurance": "#6f7887", "doc-medical": "#94756c" };
    const color = colors[String(params.id)] ?? "#9b9d96";
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="240" height="300" viewBox="0 0 240 300"><rect width="240" height="300" fill="#e7e3da"/><rect x="18" y="18" width="204" height="264" fill="#fbfaf5"/><rect x="18" y="18" width="204" height="38" fill="${color}"/><rect x="38" y="82" width="130" height="7" fill="#a5a49c"/><rect x="38" y="105" width="164" height="4" fill="#d0cec5"/><rect x="38" y="120" width="150" height="4" fill="#d0cec5"/><rect x="38" y="135" width="160" height="4" fill="#d0cec5"/><rect x="38" y="180" width="90" height="45" fill="#e0ded6"/><rect x="38" y="246" width="112" height="5" fill="#c2c0b8"/></svg>`;
    return HttpResponse.text(svg, { headers: { "Content-Type": "image/svg+xml", "Cache-Control": "max-age=3600" } });
  }),
];
