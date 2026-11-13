import type { DocumentView, Hit, Taxonomy, User } from "../api";
import { defaultRoleForType } from "../organizationRoles";

export const mockUser: User = {
  id: "018f0000-0000-7000-8000-000000000001",
  username: "demo",
  display_name: "Demo user",
  locale: "en",
  is_admin: true,
};

export const taxonomy: Taxonomy = {
  types: [
    { id: "type-passport", key: "passport", labels: { en: "Passport", ru: "Паспорт" }, builtin: true, created_at: "2026-01-01T00:00:00Z", document_count: 1, fields: [
      { key: "number", labels: { en: "Document number", ru: "Номер документа" }, kind: "identifier", required: true },
      { key: "expiry_date", labels: { en: "Expiry date", ru: "Действует до" }, kind: "date", required: false },
    ] },
    { id: "type-contract", key: "rental_agreement", labels: { en: "Rental agreement", ru: "Договор аренды" }, builtin: true, created_at: "2026-01-01T00:00:00Z", document_count: 1, fields: [
      { key: "start_date", labels: { en: "Start date", ru: "Дата начала" }, kind: "date", required: false },
      { key: "amount", labels: { en: "Monthly amount", ru: "Ежемесячный платёж" }, kind: "money", required: false },
    ] },
    { id: "type-receipt", key: "receipt", labels: { en: "Receipt", ru: "Чек" }, builtin: true, created_at: "2026-01-01T00:00:00Z", document_count: 1, fields: [
      { key: "merchant", labels: { en: "Merchant", ru: "Продавец" }, kind: "text", required: false },
      { key: "total", labels: { en: "Total", ru: "Сумма" }, kind: "money", required: false },
    ] },
    { id: "type-insurance", key: "insurance_policy", labels: { en: "Insurance policy", ru: "Страховой полис" }, builtin: true, created_at: "2026-01-01T00:00:00Z", document_count: 1, fields: [
      { key: "policy_number", labels: { en: "Policy number", ru: "Номер полиса" }, kind: "identifier", required: false },
    ] },
    { id: "type-medical", key: "medical_document", labels: { en: "Medical document", ru: "Медицинский документ" }, builtin: true, created_at: "2026-01-01T00:00:00Z", document_count: 1, fields: [
      { key: "clinic", labels: { en: "Clinic", ru: "Клиника" }, kind: "text", required: false },
    ] },
  ],
  people: [
    { id: "person-alex", name: "Alex Example", aliases: ["A. Example"], notes: "Synthetic demo person", created_at: "2026-01-01T00:00:00Z", document_count: 3 },
    { id: "person-morgan", name: "Morgan Sample", aliases: [], notes: "Synthetic demo person", created_at: "2026-01-01T00:00:00Z", document_count: 2 },
  ],
  correspondents: [
    { id: "corr-north", name: "North Bank", aliases: ["NB"], created_at: "2026-01-01T00:00:00Z", document_count: 1 },
    { id: "corr-city", name: "City Housing", aliases: [], created_at: "2026-01-01T00:00:00Z", document_count: 1 },
    { id: "corr-market", name: "Neighbourhood Market", aliases: [], created_at: "2026-01-01T00:00:00Z", document_count: 1 },
  ],
  tags: [
    { id: "tag-warranty", name: "warranty", color: "#63765a", document_count: 1 },
    { id: "tag-home", name: "home", color: "#8a6845", document_count: 2 },
    { id: "tag-renew", name: "renew", color: "#7b5c52", document_count: 1 },
  ],
};

export const hits: Hit[] = [
  {
    id: "doc-passport",
    title: "Passport — Alex Example",
    type_key: "passport",
    document_date: "2024-03-14",
    due_date: "2034-03-13",
    organizations: ["Civil Registry"],
    people: ["Alex Example"],
    key_fields: {
      number: { kind: "identifier", value: "P00012345" },
      expiry_date: { kind: "date", value: "2034-03-13" },
    },
    snippet: "Machine-readable passport. Synthetic specimen for the archive demo.",
  },
  {
    id: "doc-rental",
    title: "Apartment rental agreement",
    type_key: "rental_agreement",
    document_date: "2025-09-01",
    organizations: ["City Housing"],
    people: ["Alex Example", "Morgan Sample"],
    key_fields: {
      start_date: { kind: "date", value: "2025-09-15" },
      amount: { kind: "money", value: { amount: "1250.00", currency: "EUR" } },
    },
    snippet: "The agreement begins on 15 September and renews automatically.",
  },
  {
    id: "doc-receipt",
    title: "Vacuum cleaner receipt",
    type_key: "receipt",
    document_date: "2026-01-18",
    organizations: ["Neighbourhood Market"],
    people: ["Morgan Sample"],
    key_fields: {
      merchant: { kind: "text", value: "Neighbourhood Market" },
      total: { kind: "money", value: { amount: "189.90", currency: "EUR" } },
    },
    snippet: "Two-year warranty. Keep this receipt for service requests.",
  },
  {
    id: "doc-insurance",
    title: "Home insurance policy",
    type_key: "insurance_policy",
    document_date: "2025-12-02",
    organizations: ["North Bank"],
    people: ["Alex Example"],
    key_fields: { policy_number: { kind: "identifier", value: "HOME-2025-0042" } },
    snippet: "Annual household policy covering the apartment and belongings.",
  },
  {
    id: "doc-medical",
    title: "Clinic visit summary",
    type_key: "medical_document",
    document_date: "2026-02-11",
    attention: ["missing_fields"],
    organizations: ["Community Clinic"],
    people: ["Morgan Sample"],
    key_fields: { clinic: { kind: "text", value: "Community Clinic" } },
    snippet: "Routine visit summary. Synthetic text for interface testing.",
  },
];

export const documentTags: Record<string, string[]> = {
  "doc-passport": ["renew"],
  "doc-rental": ["home"],
  "doc-receipt": ["warranty", "home"],
  "doc-insurance": ["home"],
  "doc-medical": [],
};

export const attentionDocuments = new Set(["doc-medical"]);

/** Documents `has:expiring` finds; empty unless a test fills it. */
export const expiringDocuments = new Set<string>();

export const documentViews: Record<string, DocumentView> = Object.fromEntries(hits.map((hit) => {
  const type = taxonomy.types.find((item) => item.key === hit.type_key);
  const pageCount = hit.id === "doc-passport" || hit.id === "doc-rental" ? 2 : 1;
  const document: DocumentView = {
    id: hit.id,
    title: hit.title,
    title_provenance: "llm",
    document_date: hit.document_date,
    date_provenance: "llm",
    due_date: hit.due_date ?? null,
    type_id: type?.id ?? null,
    type_key: hit.type_key,
    type_provenance: "llm",
    organizations: hit.organizations.map((name) => {
      const correspondent = taxonomy.correspondents.find((item) => item.name === name);
      return {
        id: correspondent?.id ?? `corr-${hit.id}`,
        name,
        role: defaultRoleForType(hit.type_key),
        provenance: "llm",
      };
    }),
    summary: hit.snippet,
    summary_provenance: "llm",
    language: "en",
    local_only: false,
    status: "ready",
    status_reason: null,
    attention: attentionDocuments.has(hit.id) ? ["missing_fields"] : [],
    added_by: mockUser.id,
    updated_by: mockUser.id,
    created_at: "2026-02-12T09:00:00Z",
    updated_at: "2026-02-12T09:05:00Z",
    trashed_at: null,
    file: {
      sha256: `synthetic-${hit.id}`,
      mime: "application/pdf",
      size: 184320,
      original_name: `${hit.id}.pdf`,
      page_count: pageCount,
      has_pages: true,
      download_name: `${hit.title}.pdf`,
      original_download_name: `${hit.title}.pdf`,
      clean_size: hit.id === "doc-passport" ? 412_000 : null,
    },
    people: hit.people.map((name) => ({
      id: taxonomy.people.find((person) => person.name === name)?.id ?? `person-${hit.id}`,
      name,
      provenance: "llm",
    })),
    tags: (documentTags[hit.id] ?? []).map((name) => ({
      id: taxonomy.tags.find((tag) => tag.name === name)?.id ?? `tag-${name}`,
      name,
      provenance: "manual",
    })),
    fields: Object.entries(hit.key_fields).map(([key, value], index) => ({
      key,
      value,
      provenance: hit.id === "doc-passport" ? "mrz" : "llm",
      page: index % pageCount + 1,
    })),
    facts: [
      { key: "archive_reference", value: `DEMO-${hit.id.slice(4).toUpperCase()}`, provenance: "local", page: 1 },
      { key: "registration_removed", label: "Снят с регистрационного учёта", value: "23.07.2024", provenance: "llm", page: 1 },
    ],
    text: {
      page: 1,
      page_count: pageCount,
      source: "embedded",
      text: `${hit.title}\n${hit.snippet}\nSynthetic fixture. No real personal data.`,
    },
  };
  return [hit.id, document];
}));
