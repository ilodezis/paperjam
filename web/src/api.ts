export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
    readonly details?: unknown,
    readonly documentId?: string,
  ) {
    super(message);
  }
}

export async function call<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(`/api/v1${path}`, { credentials: "same-origin", ...init });
  if (response.status === 204) return undefined as T;
  const body: unknown = await response.json().catch(() => ({}));
  if (!response.ok) {
    const error = body as { code?: string; message?: string; details?: unknown; document_id?: string };
    throw new ApiError(response.status, error.code ?? "unknown", error.message ?? response.statusText, error.details, error.document_id);
  }
  return body as T;
}

export interface UploadOptions {
  localOnly: boolean;
  typeId?: string;
  people?: string[];
  tags?: string[];
}

/** Multipart upload with progress. The option fields go before the file: the server streams the file and rejects options that follow it. */
export function uploadDocument(file: File, options: UploadOptions, onProgress?: (fraction: number) => void): Promise<Uploaded> {
  const form = new FormData();
  form.append("local_only", String(options.localOnly));
  if (options.typeId) form.append("type_id", options.typeId);
  for (const person of options.people ?? []) form.append("people", person);
  for (const tag of options.tags ?? []) form.append("tags", tag);
  form.append("file", file, file.name);
  return new Promise<Uploaded>((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open("POST", "/api/v1/documents");
    xhr.withCredentials = true;
    xhr.upload.onprogress = (event) => {
      if (event.lengthComputable && event.total > 0) onProgress?.(event.loaded / event.total);
    };
    xhr.onload = () => {
      const body = parseBody(xhr.responseText);
      if (xhr.status >= 200 && xhr.status < 300) {
        resolve(body as Uploaded);
        return;
      }
      const error = body as { code?: string; message?: string; details?: unknown; document_id?: string };
      reject(new ApiError(xhr.status, error.code ?? "unknown", error.message ?? xhr.statusText, error.details, error.document_id));
    };
    xhr.onerror = () => reject(new TypeError("network"));
    xhr.send(form);
  });
}

function parseBody(text: string): unknown {
  try {
    return JSON.parse(text) as unknown;
  } catch {
    return {};
  }
}

export interface User {
  id: string;
  username: string;
  display_name: string;
  locale: string;
  is_admin: boolean;
}

export interface ManagedUser extends User {
  person_id: string | null;
  created_at: string;
  disabled_at: string | null;
}

export interface NewUser {
  username: string;
  display_name?: string;
  password: string;
  locale: string;
  is_admin: boolean;
}

/** An OAuth client the user connected, such as Claude. */
export interface Connection {
  id: string;
  client_id: string;
  client_name: string;
  created_at: string;
  last_used_at: string | null;
}

export interface TokenView {
  id: string;
  user_id: string;
  name: string;
  prefix: string;
  created_at: string;
  last_used_at: string | null;
  revoked_at: string | null;
}

export interface CalendarFeedState {
  active: boolean;
  feed: { created_at: string; last_used_at: string | null } | null;
  lead_days: number;
}

export interface CreatedCalendarFeed {
  feed: { created_at: string; last_used_at: string | null };
  secret: string;
  path: string;
  url: string | null;
}

export interface PasskeyView {
  id: string;
  credential_id: string;
  label: string;
  created_at: string;
  last_used_at: string | null;
}

export interface PasskeyList {
  available: boolean;
  rp_id: string | null;
  user_handle: string;
  passkeys: PasskeyView[];
}

export interface PasskeyChallenge {
  ceremony: string;
  options: Record<string, unknown>;
}

export interface Stats {
  documents: number;
  by_status: Record<string, number>;
  attention: number;
  queue: { queued: number; running: number; dead: number };
  llm_month_usd: string;
}

export type FieldKind = "text" | "date" | "money" | "number" | "identifier" | "list";
export type FieldValue =
  | { kind: "text" | "date" | "identifier"; value: string }
  | { kind: "number"; value: string }
  | { kind: "money"; value: { amount: string; currency: string } }
  | { kind: "list"; value: string[] };

export interface Hit {
  id: string;
  title: string;
  type_key: string | null;
  document_date: string | null;
  /** When the document has to be renewed or replaced (an expiry date, or a Russian internal passport's 20th/45th birthday). */
  due_date?: string | null;
  organizations: string[];
  people: string[];
  key_fields: Record<string, FieldValue>;
  snippet: string;
  /** Attention flags of the document (the same codes as on its page); empty when all is well. */
  attention?: string[];
  /** The document has page images and a thumbnail; false for a plain-text file or one that failed before its pages were rendered. */
  has_pages?: boolean;
}

export interface SearchPage {
  hits: Hit[];
  total: number;
  next_cursor: string | null;
  /** No document matched every word: these match some of them. */
  partial?: boolean;
}

export interface SearchOptions {
  query: string;
  sort?: "relevance" | "date_desc" | "date_asc";
  cursor?: string | null;
  includeTrashed?: boolean;
}

export type DocumentStatus = "queued" | "processing" | "ready" | "failed";

export interface Uploaded {
  id: string;
  status: DocumentStatus;
  restored: boolean;
}

export type Provenance = "llm" | "mrz" | "manual" | "local";

export interface PersonRef {
  id: string;
  name: string;
  provenance: Provenance;
}

export interface TagRef {
  id: string;
  name: string;
  provenance: Provenance;
}

export type OrganizationRole = "issued_by" | "received_from" | "party" | "employer" | "unspecified";

export interface DocumentOrganization {
  id: string;
  name: string;
  role: OrganizationRole;
  provenance: Provenance | null;
}

export interface FieldEntry {
  key: string;
  value: FieldValue;
  provenance: Provenance;
  page: number | null;
  /** Attention flags this value raises; saving it by hand clears them. */
  issues?: string[];
}

export interface Fact {
  key: string;
  /** What the fact is, in the uploader's language (written by the model). */
  label?: string | null;
  value: string;
  provenance: Provenance;
  page: number | null;
}

export interface PagedText {
  page: number;
  page_count: number;
  source: string;
  text: string;
}

export interface DocumentView {
  id: string;
  title: string | null;
  title_provenance: Provenance | null;
  document_date: string | null;
  date_provenance: Provenance | null;
  /** When the document has to be renewed or replaced; derived from its fields. */
  due_date?: string | null;
  type_id: string | null;
  type_key: string | null;
  type_provenance: Provenance | null;
  organizations: DocumentOrganization[];
  summary: string | null;
  summary_provenance: Provenance | null;
  language: string | null;
  local_only: boolean;
  status: DocumentStatus;
  status_reason: string | null;
  attention: string[];
  added_by: string | null;
  updated_by: string | null;
  created_at: string;
  updated_at: string;
  trashed_at: string | null;
  file: {
    sha256: string;
    mime: string;
    size: number;
    original_name: string;
    page_count: number | null;
    /** Page images and a thumbnail exist; false for a plain-text file or one that failed before its pages were rendered. */
    has_pages: boolean;
    /** What a download of the document is called: its title, `.pdf` when it serves the clean copy. */
    download_name: string;
    /** What a download of the original is called. */
    original_download_name: string;
    /** Bytes of the clean copy of a scan (pages upright, cut out, straightened), when there is one. */
    clean_size: number | null;
  };
  people: PersonRef[];
  tags: TagRef[];
  fields: FieldEntry[];
  facts: Fact[];
  text: PagedText | null;
}

export interface DocumentPatch {
  title?: string | null;
  document_date?: string | null;
  type_id?: string | null;
  organizations?: { id: string; role?: OrganizationRole }[];
  people?: string[];
  tags?: string[];
  fields?: Record<string, FieldValue | null>;
  summary?: string | null;
  local_only?: boolean;
  dismiss_attention?: string[];
}

export interface DownloadLink {
  url: string;
  expires_at: string;
  filename: string;
  mime_type: string;
  size: number;
}

export type ShareTtl = "15m" | "1h" | "24h";

export interface Share {
  id: string;
  url: string;
  created_at: string;
  expires_at: string;
  show_details: boolean;
}

export interface FieldDef {
  key: string;
  labels: Record<string, string>;
  kind: FieldKind;
  validator?: string | null;
  required: boolean;
  issuer?: boolean;
}

export interface TypeView {
  id: string;
  key: string;
  labels: Record<string, string>;
  fields: FieldDef[];
  builtin: boolean;
  created_at: string;
  document_count: number;
}

export interface PersonView {
  id: string;
  name: string;
  aliases: string[];
  notes: string;
  created_at: string;
  document_count: number;
}

export interface CorrespondentView {
  id: string;
  name: string;
  aliases: string[];
  created_at: string;
  document_count: number;
}

export interface TagView {
  id: string;
  name: string;
  color: string | null;
  document_count: number;
}

export interface Taxonomy {
  types: TypeView[];
  people: PersonView[];
  correspondents: CorrespondentView[];
  tags: TagView[];
}

export interface NewPerson {
  name: string;
  aliases?: string[];
  notes?: string;
}

export interface NewCorrespondent {
  name: string;
  aliases?: string[];
}

export interface NewType {
  key: string;
  labels: Record<string, string>;
  fields: FieldDef[];
}

const json = (body: unknown, method = "POST"): RequestInit => ({
  method,
  headers: { "Content-Type": "application/json" },
  body: JSON.stringify(body),
});

export const api = {
  session: () => call<{ user: User }>("/auth/session"),
  setLocale: (locale: string) => call<{ user: User }>("/auth/session", json({ locale }, "PATCH")),
  changePassword: (current_password: string, new_password: string) =>
    call<void>("/auth/password", json({ current_password, new_password })),
  login: (username: string, password: string) => call<{ user: User }>("/auth/login", json({ username, password })),
  setupStatus: () => call<{ needed: boolean }>("/setup"),
  setup: (body: { code: string; username: string; password: string; locale: string }) => call<{ user: User }>("/setup", json(body)),
  logout: () => call<void>("/auth/logout", { method: "POST" }),
  passkeyLoginChallenge: (username?: string) => call<PasskeyChallenge>("/auth/passkey/challenge", json(username ? { username } : {})),
  passkeyLogin: (ceremony: string, credential: Record<string, unknown>) => call<{ user: User }>("/auth/passkey", json({ ceremony, credential })),
  passkeys: () => call<PasskeyList>("/passkeys"),
  passkeyChallenge: () => call<PasskeyChallenge>("/passkeys/challenge", { method: "POST" }),
  addPasskey: (ceremony: string, label: string, credential: Record<string, unknown>) => call<PasskeyView>("/passkeys", json({ ceremony, label, credential })),
  removePasskey: (id: string) => call<void>(`/passkeys/${id}`, { method: "DELETE" }),
  search: ({ query, sort = "relevance", cursor = null, includeTrashed = false }: SearchOptions) =>
    call<SearchPage>("/search", json({ query: query || null, filters: { include_trashed: includeTrashed }, sort, limit: 20, cursor })),
  taxonomy: () => call<Taxonomy>("/taxonomy"),
  createPerson: (input: NewPerson) => call<PersonView>("/people", json(input)),
  updatePerson: (id: string, patch: { name?: string; notes?: string }) => call<PersonView>(`/people/${id}`, json(patch, "PATCH")),
  addPersonAlias: (id: string, alias: string) => call<PersonView>(`/people/${id}/aliases`, json({ alias })),
  removePersonAlias: (id: string, alias: string) => call<PersonView>(`/people/${id}/aliases?alias=${encodeURIComponent(alias)}`, { method: "DELETE" }),
  mergePeople: (id: string, from: string) => call<PersonView>(`/people/${id}/merge`, json({ from })),
  deletePerson: (id: string) => call<void>(`/people/${id}`, { method: "DELETE" }),
  createCorrespondent: (input: NewCorrespondent) => call<CorrespondentView>("/correspondents", json(input)),
  updateCorrespondent: (id: string, patch: { name?: string }) => call<CorrespondentView>(`/correspondents/${id}`, json(patch, "PATCH")),
  addCorrespondentAlias: (id: string, alias: string) => call<CorrespondentView>(`/correspondents/${id}/aliases`, json({ alias })),
  removeCorrespondentAlias: (id: string, alias: string) => call<CorrespondentView>(`/correspondents/${id}/aliases?alias=${encodeURIComponent(alias)}`, { method: "DELETE" }),
  mergeCorrespondents: (id: string, from: string) => call<CorrespondentView>(`/correspondents/${id}/merge`, json({ from })),
  deleteCorrespondent: (id: string) => call<void>(`/correspondents/${id}`, { method: "DELETE" }),
  createTag: (input: { name: string; color?: string | null }) => call<TagView>("/tags", json(input)),
  renameTag: (id: string, name: string) => call<TagView>(`/tags/${id}`, json({ name }, "PATCH")),
  deleteTag: (id: string) => call<void>(`/tags/${id}`, { method: "DELETE" }),
  createType: (input: NewType) => call<TypeView>("/types", json(input)),
  updateType: (id: string, patch: { labels?: Record<string, string>; fields?: FieldDef[] }) => call<TypeView>(`/types/${id}`, json(patch, "PATCH")),
  document: (id: string, textPage?: number) => call<DocumentView>(`/documents/${id}${textPage ? `?text_page=${textPage}` : ""}`),
  patchDocument: (id: string, patch: DocumentPatch) => call<DocumentView>(`/documents/${id}`, json(patch, "PATCH")),
  reprocess: (id: string, llm: boolean) => call<{ id: string; status: DocumentStatus }>(`/documents/${id}/reprocess`, json({ local: true, llm })),
  createDownloadLink: (id: string) => call<DownloadLink>(`/documents/${id}/download-link`, { method: "POST" }),
  shares: (id: string) => call<Share[]>(`/documents/${id}/shares`),
  createShare: (id: string, input: { ttl: ShareTtl; show_details: boolean }) => call<Share>(`/documents/${id}/shares`, json(input)),
  revokeShare: (shareId: string) => call<void>(`/shares/${shareId}`, { method: "DELETE" }),
  trashDocument: (id: string) => call<void>(`/documents/${id}/trash`, { method: "POST" }),
  restoreDocument: (id: string) => call<void>(`/documents/${id}/restore`, { method: "POST" }),
  purgeDocument: (id: string) => call<void>(`/documents/${id}`, { method: "DELETE" }),
  emptyTrash: () => call<{ purged: number }>("/trash", { method: "DELETE" }),
  users: () => call<ManagedUser[]>("/users"),
  createUser: (input: NewUser) => call<ManagedUser>("/users", json(input)),
  updateUser: (id: string, patch: { disabled?: boolean; is_admin?: boolean; display_name?: string; password?: string }) =>
    call<ManagedUser>(`/users/${id}`, json(patch, "PATCH")),
  deleteUser: (id: string) => call<void>(`/users/${id}`, { method: "DELETE" }),
  tokens: () => call<TokenView[]>("/tokens"),
  createToken: (name: string) => call<{ token: TokenView; secret: string }>("/tokens", json({ name })),
  revokeToken: (id: string) => call<void>(`/tokens/${id}`, { method: "DELETE" }),
  calendarFeed: () => call<CalendarFeedState>("/calendar-feed"),
  createCalendarFeed: () => call<CreatedCalendarFeed>("/calendar-feed", { method: "POST" }),
  revokeCalendarFeed: () => call<void>("/calendar-feed", { method: "DELETE" }),
  connections: () => call<Connection[]>("/connections"),
  disconnect: (id: string) => call<void>(`/connections/${id}`, { method: "DELETE" }),
  stats: () => call<Stats>("/stats"),
  upload: (file: File, options: UploadOptions, onProgress?: (fraction: number) => void) => uploadDocument(file, options, onProgress),
  originalUrl: (id: string) => `/api/v1/documents/${id}/original`,
  /** The clean copy of a scan when there is one, else the original. */
  downloadUrl: (id: string) => `/api/v1/documents/${id}/download`,
  /** A page of the document as it is downloaded; `original` asks for the page as it was read, before cleaning. */
  pageImageUrl: (id: string, page: number, original = false) => `/api/v1/documents/${id}/pages/${page}${original ? "-original" : ""}.webp`,
  thumbnailUrl: (id: string) => `/api/v1/documents/${id}/thumbnail.webp`,
};
