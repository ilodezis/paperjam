import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertCircle, ArrowLeft, Check, ChevronDown, ChevronLeft, ChevronRight, CloudOff, Download, EllipsisVertical, FileDown, FileText, LoaderCircle, Plus, Layers, RefreshCw, RotateCw, Share2, Trash2, X } from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { useTranslation } from "react-i18next";

import { ApiError, api, type DocumentPatch, type DocumentView, type Fact, type FieldDef, type FieldEntry, type OrganizationRole, type Provenance, type Taxonomy } from "../api";
import { ORGANIZATION_ROLES, defaultRoleForType, organizationsHeadingKey } from "../organizationRoles";
import { ShareSheet } from "../components/ShareSheet";
import { SelectDropdown } from "../components/controls/SelectDropdown";
import { toast } from "../components/controls/toast";
import { fieldFromInput, hasPageImages, toggleId } from "../document";
import { dueState, formatFileSize, formatIsoDate } from "../format";
import { documentEventsQuery, documentQuery, invalidateArchive, settingsSummaryQuery, taxonomyQuery } from "../queries";
import { documentIdOf } from "../shell/nav";
import { ScreenLink, hrefOf, useGo, useScreenBack } from "../shell/navigation";
import { TopBar } from "../shell/PageHead";
import { useScreen } from "../shell/ScreenFrame";
import { Sheet } from "../shell/Sheet";

/** Words written in capitals when a key without a label is shown. */
const ABBREVIATIONS = new Set(["id", "inn", "iban", "bic", "swift", "vat", "mrz", "idnp", "snils", "ogrn", "kpp", "eu", "uk", "us"]);

/** A readable label for a field or fact key the document type does not name: `tax_id_inn` → `Tax ID INN`. */
export function humanizeKey(key: string): string {
  const words = key.split(/[_\s]+/).filter(Boolean).map((word) => ABBREVIATIONS.has(word.toLowerCase()) ? word.toUpperCase() : word);
  const text = words.join(" ");
  return text.charAt(0).toUpperCase() + text.slice(1);
}

/** Processing failure codes the server sends in `status_reason`; each has its own sentence in the locales. */
const FAILURE_REASON_CODES = new Set(["pdf_corrupt", "pdf_encrypted", "image_corrupt", "image_too_large", "heic_unsupported", "ocr_failed", "ocr_timeout", "worker_failed", "render_failed", "processing_failed", "llm_failed", "file_too_large"]);

/** The locale key that explains a failure code; any code the page does not know gets the generic text. */
export function failureReasonKey(code: string): string {
  return FAILURE_REASON_CODES.has(code) ? code : "unknown";
}

function definitionLabel(definition: FieldDef | undefined, language: string): string | undefined {
  return definition?.labels[language] ?? definition?.labels.en;
}

function formatEventTime(iso: string, language: string): string {
  return new Intl.DateTimeFormat(language, { dateStyle: "medium", timeStyle: "short" }).format(new Date(iso));
}

function PageReference({ page, onOpen }: { page: number; onOpen: () => void }) {
  const { t } = useTranslation();
  const label = t("document.goToPage", { page });
  return <button type="button" className="page-reference" onClick={onOpen} aria-label={label} title={label}>{t("document.onPage", { page })}</button>;
}

const RAW_MRZ_KEYS = new Set(["mrz", "mrz_line", "mrz_lines", "mrz_raw", "raw_mrz"]);
const MRZ_VALUE_KEYS = new Set(["number", "nationality", "birth_date", "expiry_date", "sex", "surname", "given_names", "personal_number"]);

/** `mrz_line_1`, `mrz_line2`, `mrz_1`: the model numbers the lines of a multi-line zone. */
const NUMBERED_MRZ_KEY = /^(raw_)?mrz(_line|_raw)?_?\d$/;

function isRawMrzFact(fact: Fact): boolean {
  const key = fact.key.toLowerCase();
  if (!RAW_MRZ_KEYS.has(key) && !NUMBERED_MRZ_KEY.test(key)) return false;
  const compact = fact.value.replace(/\s/g, "");
  return fact.provenance === "mrz" || (/^[A-Z0-9<]+$/.test(compact) && compact.includes("<<"));
}

function hasExtractedMrzValues(document: DocumentView): boolean {
  return document.fields.some((entry) => entry.provenance === "mrz" || MRZ_VALUE_KEYS.has(entry.key))
    || document.facts.some((fact) => fact.provenance === "mrz" && !isRawMrzFact(fact));
}

const FIELD_GROUPS = ["dates", "amounts", "identifiers", "other"] as const;
type FieldGroup = typeof FIELD_GROUPS[number];

function fieldGroup(entry: FieldEntry): FieldGroup {
  if (entry.value.kind === "date") return "dates";
  if (entry.value.kind === "money" || entry.value.kind === "number") return "amounts";
  if (entry.value.kind === "identifier") return "identifiers";
  return "other";
}

/** Flags that reprocessing can resolve. */
const REPROCESS_FLAGS = new Set(["llm_failed", "llm_budget", "processing_failed", "mrz_mismatch", "illegible"]);

/** Flags that can be dismissed directly from the attention list. */
const DISMISSIBLE_FLAGS = new Set(["mrz_mismatch", "llm_failed", "llm_budget", "processing_failed", "illegible"]);

// Typed values save like the metadata above them: when focus leaves the field, or on Enter.
function FieldValueInput({ entry, definition, onSave }: {
  entry: FieldEntry;
  definition?: FieldDef;
  onSave: (patch: DocumentPatch) => Promise<void>;
}) {
  const { t, i18n } = useTranslation();
  const initialText = entry.value.kind === "money" ? entry.value.value.amount : entry.value.kind === "list" ? entry.value.value.join(", ") : entry.value.value;
  const initialCurrency = entry.value.kind === "money" ? entry.value.value.currency : "EUR";
  const [text, setText] = useState(initialText);
  const [currency, setCurrency] = useState(initialCurrency);
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const language = i18n.resolvedLanguage ?? "en";
  const label = definitionLabel(definition, language) ?? humanizeKey(entry.key);
  const inputType = entry.value.kind === "date" ? "date" : entry.value.kind === "number" || entry.value.kind === "money" ? "number" : "text";
  const issues = entry.issues ?? [];

  async function store(value: string, valueCurrency: string) {
    setSaving(true);
    try {
      await onSave({ fields: { [entry.key]: fieldFromInput(entry.value.kind, value, valueCurrency) } });
      setDirty(false);
    } catch {
      setDirty(true);
    } finally {
      setSaving(false);
    }
  }

  function save() {
    if (dirty && !saving) void store(text, currency);
  }

  function saveOnEnter(event: ReactKeyboardEvent<HTMLInputElement>) {
    if (event.key !== "Enter") return;
    event.preventDefault();
    save();
  }

  return (
    <>
      <form
        className={issues.length > 0 ? "typed-field typed-field--issue" : "typed-field"}
        onSubmit={(event) => { event.preventDefault(); save(); }}
        onBlur={(event) => { if (!event.currentTarget.contains(event.relatedTarget as Node | null)) save(); }}
      >
        <label>
          <span>{label}{definition?.required ? <span className="required-mark"> *</span> : null}</span>
          <span className="typed-field-inputs">
            <input
              type={inputType}
              step={inputType === "number" ? "any" : undefined}
              value={text}
              onChange={(event) => { setText(event.target.value); setDirty(true); }}
              onKeyDown={saveOnEnter}
              aria-label={label}
            />
            {entry.value.kind === "money" ? <input className="currency-input" value={currency} maxLength={3} onChange={(event) => { setCurrency(event.target.value); setDirty(true); }} onKeyDown={saveOnEnter} aria-label={t("document.currency")} /> : null}
          </span>
        </label>
        <span className="typed-field-status">{saving ? <LoaderCircle className="spin" aria-label={t("document.saving")} /> : null}</span>
      </form>
      {issues.length > 0 && !dirty ? (
        <div className="field-issue">
          <AlertCircle aria-hidden="true" />
          <span>{issues.map((flag) => t(`document.fieldIssues.${flag}`, { defaultValue: flag })).join(" ")}</span>
          <button type="button" className="quiet-button" disabled={saving} onClick={() => void store(initialText, initialCurrency)} aria-label={t("document.acceptValueFor", { field: label })}><Check aria-hidden="true" />{t("document.acceptValue")}</button>
        </div>
      ) : null}
    </>
  );
}

/** Past this many characters a description (a lease, a long letter) starts folded to a few lines. */
const LONG_DESCRIPTION = 600;

/** The description as text, in full, edited in the same place: "Edit" turns the text into a field
 *  of the same size; leaving the field (or "Done") saves, Escape puts the old text back. */
function DescriptionSection({ summary, onSave }: { summary: string | null; onSave: (summary: string | null) => void }) {
  const { t } = useTranslation();
  const [editing, setEditing] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const field = useRef<HTMLTextAreaElement>(null);
  const text = summary ?? "";
  const long = text.length > LONG_DESCRIPTION;

  function fit(node: HTMLTextAreaElement | null) {
    if (!node) return;
    node.style.height = "auto";
    node.style.height = `${node.scrollHeight + 2}px`;
  }

  useLayoutEffect(() => {
    if (!editing) return;
    const node = field.current;
    fit(node);
    node?.focus();
    node?.setSelectionRange(node.value.length, node.value.length);
  }, [editing]);

  function commit() {
    const value = field.current?.value.trim() ?? text;
    setEditing(false);
    if (value !== text) onSave(value || null);
  }

  return (
    <section className="metadata-section description-section">
      <div className="metadata-section-heading">
        <h2>{t("document.description")}</h2>
        <button
          type="button"
          className="quiet-button description-edit"
          aria-pressed={editing}
          onMouseDown={(event) => { if (editing) event.preventDefault(); }}
          onClick={() => (editing ? commit() : setEditing(true))}
        >
          {editing ? t("document.doneEditing") : t("document.editDescription")}
        </button>
      </div>
      {editing ? (
        <textarea
          ref={field}
          className="description-field"
          defaultValue={text}
          rows={3}
          placeholder={t("document.summaryPlaceholder")}
          aria-label={t("document.summary")}
          onInput={(event) => fit(event.currentTarget)}
          onBlur={commit}
          onKeyDown={(event) => {
            if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); setEditing(false); }
            if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) { event.preventDefault(); commit(); }
          }}
        />
      ) : (
        <>
          <p
            className={["description-text", text ? "" : "description-text--empty", long && !expanded ? "description-text--folded" : ""].filter(Boolean).join(" ")}
            onDoubleClick={() => setEditing(true)}
          >
            {text || t("document.summaryPlaceholder")}
          </p>
          {long ? <button type="button" className="quiet-button description-more" onClick={() => setExpanded((value) => !value)}>{expanded ? t("document.showLess") : t("document.showAll")}</button> : null}
        </>
      )}
    </section>
  );
}

/** When the document has to be renewed or replaced, and how near that is. */
export function DueDate({ date, language }: { date: string; language: string }) {
  const { t } = useTranslation();
  const due = dueState(date);
  const hint = due?.state === "overdue" ? t("document.dueOverdue") : due?.state === "soon" ? t("document.dueIn", { count: due.days }) : null;
  return (
    <div className={due && due.state !== "later" ? "metadata-field due-date due-date--soon" : "metadata-field due-date"}>
      <span>{t("document.due")}</span>
      <p><time dateTime={date}>{formatIsoDate(date, language)}</time>{hint ? <small>{hint}</small> : null}</p>
    </div>
  );
}

/** The name a person typed, as the server will store it: edges trimmed, inner whitespace collapsed. */
export function typedName(text: string): string {
  return text.trim().replace(/\s+/g, " ");
}

/** Whether `name` is already one of the options, ignoring case and spacing: then there is nothing to create. */
export function hasOptionNamed(options: { name: string }[], name: string): boolean {
  const wanted = typedName(name).toLocaleLowerCase();
  return options.some((option) => typedName(option.name).toLocaleLowerCase() === wanted);
}

function PeopleTagsPicker({ kind, document, taxonomy, onSave }: {
  kind: "people" | "tags";
  document: DocumentView;
  taxonomy: Taxonomy;
  onSave: (patch: DocumentPatch) => Promise<void>;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [typed, setTyped] = useState("");
  const [creating, setCreating] = useState(false);
  const options = taxonomy[kind];
  const selected = document[kind];
  const ids = selected.map((item) => item.id);
  const name = typedName(typed);
  const needle = name.toLocaleLowerCase();
  const shown = needle === "" ? options : options.filter((option) => option.name.toLocaleLowerCase().includes(needle));
  const canCreate = name !== "" && !hasOptionNamed(options, name);

  // A new person or tag is created first, then put on this document; the archive's lists and counts
  // are refreshed the way every other edit on this page refreshes them.
  async function create() {
    if (!canCreate || creating) return;
    setCreating(true);
    try {
      const made = kind === "people" ? await api.createPerson({ name }) : await api.createTag({ name });
      await onSave({ [kind]: [...ids, made.id] });
      setTyped("");
    } catch {
      // The page shows the error code of a failed save; what was typed stays for another try. A
      // creation that failed has no error shown by the save, so it is reported here.
      toast(t("document.createFailed"), { icon: "alert" });
    } finally {
      setCreating(false);
      void invalidateArchive(queryClient);
    }
  }

  return (
    <details className="metadata-picker">
      <summary>
        <span>{t(`document.${kind}`)}</span>
        <span>{selected.length > 0 ? selected.map((item) => item.name).join(", ") : t(kind === "people" ? "document.peopleNone" : "document.noTagsSelected")}<ChevronDown className="disclosure-chevron" aria-hidden="true" /></span>
      </summary>
      <div>
        <input
          type="text"
          className="metadata-picker-input"
          value={typed}
          maxLength={100}
          autoComplete="off"
          spellCheck={false}
          placeholder={t(kind === "people" ? "document.findOrAddPerson" : "document.findOrAddTag")}
          aria-label={t(kind === "people" ? "document.findOrAddPerson" : "document.findOrAddTag")}
          onChange={(event) => setTyped(event.target.value)}
          onKeyDown={(event) => {
            if (event.key !== "Enter") return;
            event.preventDefault();
            void create();
          }}
        />
        {shown.map((option) => (
          <label key={option.id}>
            <input
              type="checkbox"
              checked={ids.includes(option.id)}
              onChange={(event) => void onSave({ [kind]: toggleId(ids, option.id, event.target.checked) })}
            />
            <span>{option.name}</span><small>{t("document.usedBy", { count: option.document_count })}</small>
          </label>
        ))}
        {canCreate ? (
          <button type="button" className="metadata-picker-create" disabled={creating} onClick={() => void create()}>
            <Plus aria-hidden="true" />{t("document.createNamed", { name })}
          </button>
        ) : null}
      </div>
    </details>
  );
}

interface DraftOrg {
  key: string;
  id: string;
  role: OrganizationRole;
}

export function OrganizationsEditor({
  document,
  taxonomy,
  onSave,
}: {
  document: DocumentView;
  taxonomy?: Taxonomy;
  onSave: (patch: DocumentPatch) => Promise<void>;
}) {
  const { t } = useTranslation();
  const [drafts, setDrafts] = useState<DraftOrg[]>([]);
  const [saveVersion, setSaveVersion] = useState(0);

  const headingKey = organizationsHeadingKey(document.organizations, document.type_key);
  const roleOptions = ORGANIZATION_ROLES.map((role) => ({ value: role, label: t(`document.organizationRoles.${role}`) }));

  function addDraft() {
    setDrafts((current) => [
      ...current,
      {
        key: `draft-${Date.now()}-${Math.random()}`,
        id: "",
        role: defaultRoleForType(document.type_key),
      },
    ]);
  }

  function removeDraft(key: string) {
    setDrafts((current) => current.filter((d) => d.key !== key));
  }

  function updateDraftRole(key: string, role: OrganizationRole) {
    setDrafts((current) =>
      current.map((d) => (d.key === key ? { ...d, role } : d)),
    );
  }

  async function handleOrgChange(index: number, newOrgId: string) {
    if (!newOrgId || newOrgId === document.organizations[index]?.id) return;
    const next = document.organizations.map((org, i) =>
      i === index ? { id: newOrgId, role: org.role } : { id: org.id, role: org.role },
    );
    try {
      await onSave({ organizations: next });
    } catch {
      setSaveVersion((v) => v + 1);
    }
  }

  async function handleRoleChange(index: number, newRole: OrganizationRole) {
    if (newRole === document.organizations[index]?.role) return;
    const next = document.organizations.map((org, i) =>
      i === index ? { id: org.id, role: newRole } : { id: org.id, role: org.role },
    );
    try {
      await onSave({ organizations: next });
    } catch {
      setSaveVersion((v) => v + 1);
    }
  }

  async function handleRemove(index: number) {
    const next = document.organizations
      .filter((_, i) => i !== index)
      .map((org) => ({ id: org.id, role: org.role }));
    try {
      await onSave({ organizations: next });
    } catch {
      setSaveVersion((v) => v + 1);
    }
  }

  async function handleDraftOrgChange(key: string, orgId: string, role: OrganizationRole) {
    if (!orgId) return;
    const currentSaved = document.organizations.map((o) => ({ id: o.id, role: o.role }));
    const next = [...currentSaved, { id: orgId, role }];
    try {
      await onSave({ organizations: next });
      setDrafts((current) => current.filter((d) => d.key !== key));
    } catch {
      setDrafts((current) =>
        current.map((d) => (d.key === key ? { ...d, id: "" } : d)),
      );
      setSaveVersion((v) => v + 1);
    }
  }

  return (
    <div className="metadata-field organizations-editor">
      <span>{t(headingKey)}</span>
      {document.organizations.map((org, index) => {
        const missingFromTaxonomy = !taxonomy?.correspondents.some((c) => c.id === org.id);
        return (
          <div key={`${org.id}-${index}`} className="organization-row">
            <SelectDropdown
              key={`org-${index}-${org.id}-${saveVersion}`}
              className="organization-row-select"
              value={org.id}
              aria-label={t("document.organizationSelect")}
              onChange={(value) => void handleOrgChange(index, value)}
              options={[
                ...(missingFromTaxonomy && org.id ? [{ value: org.id, label: org.name }] : []),
                ...(taxonomy?.correspondents ?? []).map((corr) => ({ value: corr.id, label: corr.name })),
              ]}
            />
            <SelectDropdown
              key={`role-${index}-${org.role}-${saveVersion}`}
              className="organization-row-role"
              value={org.role}
              aria-label={t("document.organizationRoleSelect")}
              onChange={(value) => void handleRoleChange(index, value as OrganizationRole)}
              options={roleOptions}
            />
            <button
              type="button"
              className="organization-row-remove"
              aria-label={t("document.removeOrganization")}
              title={t("document.removeOrganization")}
              onClick={() => void handleRemove(index)}
            >
              <X aria-hidden="true" />
            </button>
          </div>
        );
      })}
      {drafts.map((draft) => (
        <div key={draft.key} className="organization-row">
          <SelectDropdown
            key={`draft-org-${draft.key}-${draft.id}-${saveVersion}`}
            className="organization-row-select"
            value={draft.id}
            aria-label={t("document.organizationSelect")}
            onChange={(value) => void handleDraftOrgChange(draft.key, value, draft.role)}
            placeholder={t("document.organizationNotChosen")}
            options={(taxonomy?.correspondents ?? []).map((corr) => ({ value: corr.id, label: corr.name }))}
          />
          <SelectDropdown
            key={`draft-role-${draft.key}-${draft.role}-${saveVersion}`}
            className="organization-row-role"
            value={draft.role}
            aria-label={t("document.organizationRoleSelect")}
            onChange={(value) => updateDraftRole(draft.key, value as OrganizationRole)}
            options={roleOptions}
          />
          <button
            type="button"
            className="organization-row-remove"
            aria-label={t("document.removeOrganization")}
            title={t("document.removeOrganization")}
            onClick={() => removeDraft(draft.key)}
          >
            <X aria-hidden="true" />
          </button>
        </div>
      ))}
      {document.organizations.length === 0 && drafts.length === 0 ? (
        <p className="metadata-empty">{t("document.organizationsNone")}</p>
      ) : null}
      <button
        type="button"
        className="button button--secondary organization-add-button"
        onClick={addDraft}
      >
        <Plus aria-hidden="true" />
        {t("document.addOrganization")}
      </button>
    </div>
  );
}

export function DocumentPage() {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();
  const { path, active } = useScreen();
  const id = documentIdOf(path) ?? "";
  const go = useGo();
  const goBack = useScreenBack();
  const [page, setPage] = useState(1);
  const [failedImage, setFailedImage] = useState<string | null>(null);
  const [loadedImage, setLoadedImage] = useState<string | null>(null);
  const [natural, setNatural] = useState<{ url: string; width: number; height: number } | null>(null);
  // Width over height of page 1, known from its thumbnail before the page itself arrives.
  const [thumbnailRatio, setThumbnailRatio] = useState<{ id: string; ratio: number } | null>(null);
  const [rotations, setRotations] = useState<Record<number, number>>({});
  // A scan is shown from its clean copy; this swaps in the pages as they were read.
  const [showOriginal, setShowOriginal] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [shareOpen, setShareOpen] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const saveVersion = useRef(0);
  const actionMenu = useRef<HTMLDetailsElement>(null);
  const attentionSection = useRef<HTMLElement>(null);
  const language = i18n.resolvedLanguage ?? "en";

  const { data: document, error: queryError, isPending } = useQuery({ ...documentQuery(id, page), placeholderData: keepPreviousData });
  const { data: events } = useQuery(documentEventsQuery(id));
  const { data: taxonomy } = useQuery(taxonomyQuery);
  const { data: settingsSummary } = useQuery(settingsSummaryQuery);
  const error = queryError instanceof ApiError ? queryError.code : queryError ? "network" : null;

  const failed = (cause: unknown) => setActionError(cause instanceof ApiError ? cause.code : "network");

  // A saved edit replaces the cached document; the archive's card for it is refreshed behind.
  const patch = useMutation({
    mutationFn: (changes: DocumentPatch) => api.patchDocument(id, changes),
    onMutate: () => {
      setActionError(null);
      return ++saveVersion.current;
    },
    onSuccess: (updated, _changes, version) => {
      // Only the newest answer is kept; an older one arriving late would put an old value back.
      if (version === saveVersion.current) queryClient.setQueryData(documentQuery(id, page).queryKey, updated);
      void queryClient.invalidateQueries({ queryKey: ["document", id], refetchType: "none" });
      void queryClient.invalidateQueries({ queryKey: ["document-events", id] });
      void queryClient.invalidateQueries({ queryKey: ["search"] });
      void queryClient.invalidateQueries({ queryKey: ["taxonomy"] });
      void queryClient.invalidateQueries({ queryKey: ["stats"] });
    },
    onError: failed,
  });

  const reprocess = useMutation({
    mutationFn: ({ llm, model }: { llm: boolean; model?: string }) =>
      model ? api.reprocess(id, llm, model) : api.reprocess(id, llm),
    onMutate: () => setActionError(null),
    onSuccess: () => {
      queryClient.setQueriesData<DocumentView>({ queryKey: ["document", id] }, (current) => (
        current ? { ...current, status: "queued", status_reason: null } : current
      ));
      void queryClient.invalidateQueries({ queryKey: ["document", id] });
      void queryClient.invalidateQueries({ queryKey: ["document-events", id] });
      void queryClient.invalidateQueries({ queryKey: ["search"] });
      void queryClient.invalidateQueries({ queryKey: ["stats"] });
      toast(t("document.reprocessQueuedToast"), { icon: "check" });
    },
    onError: failed,
  });

  const trashDocument = useMutation({
    mutationFn: () => api.trashDocument(id),
    onMutate: () => setActionError(null),
    onSuccess: () => {
      // Trashing is reversible, so the toast offers Undo instead of asking first.
      toast(t("document.trashedToast"), { icon: "check", undoLabel: t("common.undo"), undo: () => void undoTrash() });
      void invalidateArchive(queryClient);
      go(hrefOf("archive"), { mode: "tab" });
    },
    onError: failed,
  });

  const saving = patch.isPending;
  const busy = reprocess.isPending || trashDocument.isPending;

  // Escape closes the innermost open thing first: a sheet or an open disclosure, then the page.
  useEffect(() => {
    if (!active) return;
    function onKeyDown(event: KeyboardEvent) {
      if (event.key !== "Escape" || event.defaultPrevented || menuOpen || shareOpen) return;
      const target = event.target as HTMLElement | null;
      const editing = target?.matches("input, textarea, select, [contenteditable=true]") ?? false;
      if (editing) return;
      const openDisclosure = window.document.querySelector<HTMLDetailsElement>("details.action-menu[open], details.metadata-picker[open]");
      if (openDisclosure) {
        event.preventDefault();
        openDisclosure.open = false;
        openDisclosure.querySelector("summary")?.focus();
        return;
      }
      goBack();
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [active, goBack, menuOpen, shareOpen]);

  // Clicking anywhere outside the "More actions" menu closes it.
  useEffect(() => {
    if (!active) return;
    function onPointerDown(event: PointerEvent) {
      const menu = actionMenu.current;
      if (menu?.open && !menu.contains(event.target as Node | null)) menu.open = false;
    }
    window.addEventListener("pointerdown", onPointerDown);
    return () => window.removeEventListener("pointerdown", onPointerDown);
  }, [active]);

  async function savePatch(changes: DocumentPatch) {
    await patch.mutateAsync(changes);
  }

  function safeSave(changes: DocumentPatch) {
    patch.mutate(changes);
  }

  function openShare() {
    actionMenu.current?.removeAttribute("open");
    setMenuOpen(false);
    setShareOpen(true);
  }

  function reprocessDocument(llm: boolean, model?: string) {
    actionMenu.current?.removeAttribute("open");
    setMenuOpen(false);
    reprocess.mutate(model ? { llm, model } : { llm });
  }

  function trash() {
    actionMenu.current?.removeAttribute("open");
    setMenuOpen(false);
    trashDocument.mutate();
  }

  async function undoTrash() {
    try {
      await api.restoreDocument(id);
    } catch (cause) {
      failed(cause);
      return;
    }
    void invalidateArchive(queryClient);
  }

  if (!document && isPending) {
    return (
      <div className="document-page">
        <TopBar title={t("document.loading")} />
        <div className="document-loading">
          <div className="page-canvas page-canvas--loading" data-shared-target><LoaderCircle className="spin" /></div>
          <p>{t("document.loading")}</p>
        </div>
      </div>
    );
  }

  if (!document) {
    return (
      <div className="document-page">
        <TopBar title={t("document.unavailable")} />
        <div className="document-unavailable">
          <AlertCircle />
          <h1>{t("document.unavailable")}</h1>
          <p>{t(`errors.${error ?? "network"}`, { defaultValue: t("errors.document") })}</p>
          <ScreenLink className="button button--secondary" to={hrefOf("archive")} mode="tab"><ArrowLeft />{t("document.back")}</ScreenLink>
        </div>
      </div>
    );
  }

  const isProcessing = document.status === "queued" || document.status === "processing";
  const pageCount = Math.max(1, document.file.page_count ?? 1);
  const currentPage = Math.min(page, pageCount);
  const hasCleanCopy = document.file.clean_size !== null && document.file.clean_size !== undefined;
  // No page image exists for a failed or text-only file: nothing is requested, the placeholder shows.
  const pageImages = hasPageImages(document);
  const imageUrl = api.pageImageUrl(id, currentPage, hasCleanCopy && showOriginal);
  const currentType = taxonomy?.types.find((type) => type.id === document.type_id || type.key === document.type_key);
  const fieldDefinitions = new Map((currentType?.fields ?? []).map((field) => [field.key, field]));
  const title = document.title ?? document.file.original_name;
  const downloadSize = formatFileSize(document.file.clean_size ?? document.file.size, language);
  // The download is named after the title shown just below, so the line above it names only its format.
  const downloadFormat = /\.([a-z0-9]{1,5})$/i.exec(document.file.download_name)?.[1]?.toUpperCase() ?? "";
  const fieldLabel = (key: string) => definitionLabel(fieldDefinitions.get(key), language) ?? t(`document.factLabels.${key}`, { defaultValue: humanizeKey(key) });
  // A translated key wins (it follows the viewer's language), then the model's
  // label in the uploader's language, then the key made readable.
  const factLabel = (fact: Fact) => definitionLabel(fieldDefinitions.get(fact.key), language) ?? t(`document.factLabels.${fact.key}`, { defaultValue: fact.label?.trim() || humanizeKey(fact.key) });
  const flaggedFields = (flag: string) => document.fields.filter((entry) => entry.issues?.includes(flag)).map((entry) => fieldLabel(entry.key));
  const groupedFields = FIELD_GROUPS.map((group) => ({ group, entries: document.fields.filter((entry) => fieldGroup(entry) === group) })).filter(({ entries }) => entries.length > 0);
  const hideRawMrz = hasExtractedMrzValues(document);
  const visibleFacts = document.facts.filter((fact) => !(hideRawMrz && isRawMrzFact(fact)));
  const provenanceGroups = new Map<Provenance, Set<string>>();

  function addProvenance(source: Provenance | null | undefined, label: string) {
    if (!source) return;
    const labels = provenanceGroups.get(source) ?? new Set<string>();
    labels.add(label);
    provenanceGroups.set(source, labels);
  }

  addProvenance(document.title_provenance, t("document.title"));
  addProvenance(document.type_provenance, t("document.type"));
  addProvenance(document.date_provenance, t("document.date"));
  const orgsHeading = t(organizationsHeadingKey(document.organizations, document.type_key));
  document.organizations.forEach((org) => addProvenance(org.provenance, orgsHeading));
  addProvenance(document.summary_provenance, t("document.summary"));
  document.people.forEach((person) => addProvenance(person.provenance, `${t("document.people")}: ${person.name}`));
  document.tags.forEach((tag) => addProvenance(tag.provenance, `${t("document.tags")}: ${tag.name}`));
  document.fields.forEach((entry) => addProvenance(entry.provenance, fieldLabel(entry.key)));
  visibleFacts.forEach((fact) => addProvenance(fact.provenance, factLabel(fact)));
  const provenanceDetails = Array.from(provenanceGroups, ([source, labels]) => ({ source, labels: Array.from(labels) }));
  const uploadEvent = events?.find((event) => event.action === "document.upload");

  // Turning the page is a view setting only; the stored scan stays as it is.
  const rotation = rotations[currentPage] ?? 0;
  const dimensions = natural?.url === imageUrl ? natural : null;
  const rotatedRatio = dimensions && rotation !== 0 ? (rotation % 180 === 0 ? dimensions.width / dimensions.height : dimensions.height / dimensions.width) : null;
  const pageLoaded = loadedImage === imageUrl;
  // The frame is sized from the page's proportions as soon as they are known, so the sharp page
  // takes the place of its blurred thumbnail without moving anything.
  const pageRatio = dimensions ? dimensions.width / dimensions.height : currentPage === 1 && thumbnailRatio?.id === id ? thumbnailRatio.ratio : null;
  const showPlaceholder = pageImages && currentPage === 1 && !pageLoaded && failedImage !== imageUrl && rotation === 0 && !(hasCleanCopy && showOriginal);
  const pageImage = (
    <img
      key={imageUrl}
      className={pageLoaded ? "page-image page-image--loaded" : "page-image"}
      src={imageUrl}
      alt={t("document.pageImage", { current: currentPage, title })}
      style={rotatedRatio ? { transform: `translate(-50%, -50%) rotate(${rotation}deg)` } : undefined}
      onLoad={(event) => { setLoadedImage(imageUrl); setNatural({ url: imageUrl, width: event.currentTarget.naturalWidth, height: event.currentTarget.naturalHeight }); }}
      onError={() => setFailedImage(imageUrl)}
    />
  );

  const allowedModels = settingsSummary?.llm_enabled && settingsSummary.llm_allowed_models.length > 1
    ? settingsSummary.llm_allowed_models
    : [];

  function promoteFact(value: string, key: string) {
    const definition = fieldDefinitions.get(key);
    if (definition) safeSave({ fields: { [key]: fieldFromInput(definition.kind, value) } });
  }

  return (
    <div className="document-page">
      <TopBar
        title={title}
        right={
          <button type="button" className="ibtn" onClick={() => setMenuOpen(true)} aria-label={t("document.actions")} aria-haspopup="dialog">
            <EllipsisVertical aria-hidden="true" />
          </button>
        }
      />
      <header className="document-header">
        <div className="document-header-main">
          <ScreenLink to={hrefOf("archive")} mode="tab" className="document-back" aria-label={t("document.back")}><ArrowLeft /></ScreenLink>
          <div>
            <p className="eyebrow" title={document.file.original_name}>{downloadFormat ? `${downloadFormat} · ` : ""}{downloadSize}</p>
            <h1>{title}</h1>
          </div>
        </div>
        <div className="document-actions">
          <a className="button button--secondary" href={api.downloadUrl(id)} title={document.file.download_name}><Download />{t("document.download")}</a>
          <button type="button" className="button button--secondary" onClick={() => openShare()} disabled={busy}><Share2 />{t("document.share")}</button>
          <details className="action-menu" ref={actionMenu}>
            <summary className="button button--secondary"><EllipsisVertical />{t("document.moreActions")}</summary>
            <div>
              {hasCleanCopy ? <a href={api.originalUrl(id)} title={document.file.original_download_name}><FileDown aria-hidden="true" />{t("document.downloadOriginalFile")}</a> : null}
              {hasCleanCopy ? <hr /> : null}
              <p className="action-menu-label">{t("document.reprocess")}</p>
              <button type="button" onClick={() => reprocessDocument(true)} disabled={busy || isProcessing}><RefreshCw aria-hidden="true" />{t("document.reprocessLlm")}</button>
              <button type="button" onClick={() => reprocessDocument(false)} disabled={busy || isProcessing}><RefreshCw aria-hidden="true" />{t("document.reprocessLocal")}</button>
              {allowedModels.length > 0 ? (
                <>
                  <p className="action-menu-label">{t("document.reprocessWithModel")}</p>
                  {allowedModels.map((model) => (
                    <button
                      key={model}
                      type="button"
                      className="action-menu-subitem"
                      onClick={() => reprocessDocument(true, model)}
                      disabled={busy || isProcessing}
                      title={model}
                    >
                      <RefreshCw aria-hidden="true" /><span>{model}</span>
                    </button>
                  ))}
                </>
              ) : null}
              <hr />
              <button type="button" className="action-menu-danger" onClick={() => trash()} disabled={busy} aria-label={t("document.trash")}><Trash2 aria-hidden="true" />{t("document.trash")}</button>
            </div>
          </details>
        </div>
      </header>

      <div className="document-status-line">
        {!isProcessing && document.attention.length > 0 ? (
          <button type="button" className="document-status document-status--attention" onClick={() => attentionSection.current?.scrollIntoView({ behavior: "smooth", block: "center" })}>
            <AlertCircle aria-hidden="true" />{t("document.status.attention")}
          </button>
        ) : (
          <span className={`document-status document-status--${document.status}`}>
            {isProcessing ? <LoaderCircle className="spin" aria-hidden="true" /> : null}
            {t(`document.status.${document.status}`)}
          </span>
        )}
        {document.local_only ? <span><CloudOff />{t("document.localOnly")}</span> : null}
        {saving || busy ? <span><LoaderCircle className="spin" />{saving ? t("document.saving") : t("document.working")}</span> : null}
      </div>

      {document.status_reason ? <div className="document-warning" role="alert"><AlertCircle /><p>{t(`document.failureReasons.${failureReasonKey(document.status_reason)}`)}</p></div> : null}
      {actionError ? <div className="document-warning" role="alert"><AlertCircle /><p>{t(`errors.${actionError}`, { defaultValue: t("errors.document") })}</p><button type="button" className="icon-button" onClick={() => setActionError(null)} aria-label={t("common.dismiss")}><X /></button></div> : null}


      <div className="document-workspace">
        <section className="document-viewer" aria-label={t("document.viewer")}>
          {/* A file that could not be read has no pages to turn or rotate. */}
          {document.status !== "failed" ? <div className="page-controls">
            <button type="button" className="icon-button" disabled={currentPage <= 1} onClick={() => setPage(currentPage - 1)} aria-label={t("document.previousPage")}><ChevronLeft /></button>
            <span>{t("document.page", { current: currentPage, total: pageCount })}</span>
            <button type="button" className="icon-button" disabled={currentPage >= pageCount} onClick={() => setPage(currentPage + 1)} aria-label={t("document.nextPage")}><ChevronRight /></button>
            <button type="button" className="icon-button page-rotate" onClick={() => setRotations((current) => ({ ...current, [currentPage]: ((current[currentPage] ?? 0) + 90) % 360 }))} aria-label={t("document.rotate")} title={t("document.rotate")}><RotateCw /></button>
            {hasCleanCopy ? <button type="button" className="icon-button page-original" aria-pressed={showOriginal} onClick={() => setShowOriginal((current) => !current)} aria-label={t("document.showOriginal")} title={t("document.showOriginal")}><Layers /></button> : null}
          </div> : null}
          <div className="page-canvas" data-shared-target style={pageRatio && !rotatedRatio ? ({ "--page-ratio": String(pageRatio) } as CSSProperties) : undefined}>
            {pageImages && !pageLoaded && failedImage !== imageUrl ? <LoaderCircle className="spin page-canvas-spinner" aria-hidden="true" /> : null}
            {showPlaceholder ? (
              <img
                className="page-placeholder"
                src={api.thumbnailUrl(id)}
                alt=""
                aria-hidden="true"
                onLoad={(event) => { const { naturalWidth: w, naturalHeight: h } = event.currentTarget; if (w > 0 && h > 0) setThumbnailRatio({ id, ratio: w / h }); }}
              />
            ) : null}
            {!pageImages || failedImage === imageUrl ? (
              <div className="page-unavailable"><FileText /><p>{t(document.status === "failed" ? "document.noPreviewFailed" : "document.imageUnavailable")}</p><a href={api.originalUrl(id)}>{t("document.downloadOriginal")}</a></div>
            ) : rotatedRatio ? (
              <div className="page-rotated" data-sideways={rotation % 180 !== 0 ? "" : undefined} style={{ aspectRatio: String(rotatedRatio), width: `min(100%, calc(78vh * ${rotatedRatio}))` }}>{pageImage}</div>
            ) : pageImage}
          </div>
          <details className="text-layer">
            <summary><span className="text-layer-title">{t("document.extractedText")}<ChevronDown className="disclosure-chevron" aria-hidden="true" /></span>{document.text && document.text.source !== "none" ? <span>{t(`document.sources.${document.text.source}`, { defaultValue: document.text.source })}</span> : null}</summary>
            {document.text ? <pre>{document.text.text}</pre> : <p>{t("document.noText")}</p>}
          </details>
        </section>

        <aside className="document-metadata">
          <section className="metadata-section">
            <div className="metadata-section-heading"><h2>{t("document.general")}</h2><span>{t("document.inlineHint")}</span></div>
            <label className="metadata-field">
              <span>{t("document.title")}</span>
              <input key={document.title} defaultValue={document.title ?? ""} placeholder={t("document.titlePlaceholder")} onBlur={(event) => { const value = event.target.value.trim(); if (value !== (document.title ?? "")) safeSave({ title: value || null }); }} />
            </label>
            <div className="metadata-grid">
              <div className="metadata-field">
                <span>{t("document.type")}</span>
                <SelectDropdown
                  value={document.type_id ?? ""}
                  onChange={(value) => { if (value !== (document.type_id ?? "")) safeSave({ type_id: value || null }); }}
                  options={[{ value: "", label: t("document.noType") }, ...(taxonomy?.types ?? []).map((type) => ({ value: type.id, label: type.labels[language] ?? type.labels.en }))]}
                  aria-label={t("document.type")}
                />
              </div>
              {/* Not wrapped in the label: a wrapping label gives its text to the picker button and the day field too, so each is announced with the date name again. */}
              <div className="metadata-field">
                <span id={`document-date-label-${id}`}>{t("document.date")}</span>
                <input key={document.document_date} aria-labelledby={`document-date-label-${id}`} type="date" defaultValue={document.document_date ?? ""} onBlur={(event) => { if (event.target.value !== (document.document_date ?? "")) safeSave({ document_date: event.target.value || null }); }} />
              </div>
              {document.due_date ? <DueDate date={document.due_date} language={language} /> : null}
            </div>
            <OrganizationsEditor document={document} taxonomy={taxonomy} onSave={savePatch} />
            {taxonomy ? <PeopleTagsPicker kind="people" document={document} taxonomy={taxonomy} onSave={savePatch} /> : null}
            {taxonomy ? <PeopleTagsPicker kind="tags" document={document} taxonomy={taxonomy} onSave={savePatch} /> : null}
          </section>

          <DescriptionSection summary={document.summary} onSave={(summary) => safeSave({ summary })} />

          <section className="metadata-section">
            <div className="metadata-section-heading"><h2>{t("document.privacyAndProcessing")}</h2></div>
            <label className="toggle-row">
              <span className="toggle-row-text">
                <CloudOff aria-hidden="true" />
                <span><strong>{t("document.localOnly")}</strong><small>{t("document.localOnlyDescription")}</small></span>
              </span>
              <input type="checkbox" role="switch" className="switch" checked={document.local_only} onChange={(event) => safeSave({ local_only: event.target.checked })} aria-label={t("document.localOnly")} />
            </label>
            {uploadEvent ? (
              <p className="document-source-line">
                {t("document.addedViaLine", {
                  via: t(`document.addedVia.${uploadEvent.via}`, { defaultValue: uploadEvent.via }),
                  date: formatEventTime(uploadEvent.at, language),
                })}
              </p>
            ) : null}
            {provenanceDetails.length > 0 ? (
              <details className="processing-details">
                <summary><span>{t("document.automaticExtraction")}</span><span>{t("document.processingDetails")}<ChevronDown className="disclosure-chevron" aria-hidden="true" /></span></summary>
                <dl>
                  {provenanceDetails.map(({ source, labels }) => (
                    <div key={source}>
                      <dt title={t(`document.provenanceDescription.${source}`)}>{t(`document.provenance.${source}`)}</dt>
                      <dd>{labels.join(", ")}</dd>
                    </div>
                  ))}
                </dl>
              </details>
            ) : null}
            {events && events.length > 0 ? (
              <details className="processing-details document-history">
                <summary aria-label={t("document.history")}>
                  <span>{t("document.history")}</span>
                  <span>{t("document.historyCount", { count: events.length })}<ChevronDown className="disclosure-chevron" aria-hidden="true" /></span>
                </summary>
                <ul className="document-history-list">
                  {events.map((event) => {
                    const viaLabel = t(`document.eventVia.${event.via}`, { defaultValue: event.via });
                    const meta = [formatEventTime(event.at, language), viaLabel, event.actor_name].filter(Boolean).join(" · ");
                    return (
                      <li key={event.id}>
                        <strong>{t(`document.eventActions.${event.action}`, { defaultValue: event.action })}</strong>
                        <span>{meta}</span>
                      </li>
                    );
                  })}
                </ul>
              </details>
            ) : null}
          </section>

          {document.attention.length > 0 ? (
            <section className="metadata-section attention-section" ref={attentionSection}>
              <h2><AlertCircle />{t("document.attention")}</h2>
              <ul>
                {document.attention.map((flag) => {
                  const fields = flaggedFields(flag);
                  const canReprocess = REPROCESS_FLAGS.has(flag);
                  const canDismiss = DISMISSIBLE_FLAGS.has(flag) || (flag === "checksum_failed" && fields.length === 0);
                  return (
                    <li key={flag}>
                      {t(`document.attentionFlags.${flag}`, { defaultValue: t("document.attentionFlag", { flag }) })}
                      {fields.length > 0 ? <strong className="attention-fields"> {t("document.attentionFields", { fields: fields.join(", ") })}</strong> : null}
                      {canReprocess || canDismiss ? (
                        <div className="attention-actions">
                          {canReprocess ? <button type="button" className="quiet-button attention-action" disabled={busy || isProcessing} onClick={() => reprocess.mutate({ llm: true })}><RefreshCw aria-hidden="true" />{t("document.reprocess")}</button> : null}
                          {canDismiss ? <button type="button" className="quiet-button attention-action" disabled={saving || busy} onClick={() => safeSave({ dismiss_attention: [flag] })}><Check aria-hidden="true" />{t("document.dismissAttention")}</button> : null}
                        </div>
                      ) : null}
                    </li>
                  );
                })}
              </ul>
            </section>
          ) : null}

          {document.fields.length > 0 || visibleFacts.length > 0 ? (
            <section className="metadata-section">
              <div className="metadata-section-heading"><h2>{t("document.additionalFields")}</h2><span>{t("document.typedValues")}</span></div>
              {document.fields.length > 0 ? (
                <div className="typed-field-groups">
                  {groupedFields.map(({ group, entries }) => (
                    <section className="typed-field-group" key={group}>
                      <h3>{t(`document.fieldGroups.${group}`)}</h3>
                      {entries.map((entry) => (
                        <div className="typed-field-wrap" key={`${entry.key}:${JSON.stringify(entry.value)}:${entry.provenance}`}>
                          <FieldValueInput entry={entry} definition={fieldDefinitions.get(entry.key)} onSave={savePatch} />
                          {entry.page && pageCount > 1 ? <PageReference page={entry.page} onOpen={() => setPage(entry.page ?? 1)} /> : null}
                        </div>
                      ))}
                    </section>
                  ))}
                </div>
              ) : <p className="metadata-empty">{t("document.noFields")}</p>}

              {visibleFacts.length > 0 ? (
                <section className="facts-section">
                  <h3>{t("document.facts")}</h3>
                  <dl className="facts-list">
                    {visibleFacts.map((fact, index) => (
                      <div key={`${fact.key}:${index}`}>
                        <dt className="fact-dt"><span className="fact-key-name">{factLabel(fact)}</span></dt>
                        <dd className="fact-dd">
                          <span className="fact-value">{fact.value}</span>
                          {fact.page && pageCount > 1 ? <PageReference page={fact.page} onOpen={() => setPage(fact.page ?? 1)} /> : null}
                          {currentType && currentType.fields.length > 0 && !isRawMrzFact(fact) ? (
                            <SelectDropdown
                              className="fact-promote"
                              value=""
                              disabled={saving}
                              onChange={(key) => promoteFact(fact.value, key)}
                              placeholder={t("document.factToField")}
                              options={currentType.fields.map((field) => ({ value: field.key, label: fieldLabel(field.key) }))}
                              aria-label={t("document.factToFieldFor", { fact: factLabel(fact) })}
                            />
                          ) : null}
                        </dd>
                      </div>
                    ))}
                  </dl>
                </section>
              ) : null}
            </section>
          ) : null}
        </aside>
      </div>

      <Sheet open={menuOpen} title={t("document.actions")} onClose={() => setMenuOpen(false)}>
        <div className="sheet-actions">
          <a className="sheet-action" href={api.downloadUrl(id)} onClick={() => setMenuOpen(false)}><Download aria-hidden="true" />{t("document.download")}</a>
          {hasCleanCopy ? <a className="sheet-action" href={api.originalUrl(id)} onClick={() => setMenuOpen(false)}><FileDown aria-hidden="true" />{t("document.downloadOriginalFile")}</a> : null}
          <button type="button" className="sheet-action" disabled={busy} onClick={() => openShare()}><Share2 aria-hidden="true" />{t("document.share")}</button>
          <button type="button" className="sheet-action" disabled={busy || isProcessing} onClick={() => reprocessDocument(true)}><RefreshCw aria-hidden="true" />{t("document.reprocessLlm")}</button>
          <button type="button" className="sheet-action" disabled={busy || isProcessing} onClick={() => reprocessDocument(false)}><RefreshCw aria-hidden="true" />{t("document.reprocessLocal")}</button>
          {allowedModels.length > 0 ? (
            <div className="sheet-action-subgroup" role="group" aria-label={t("document.reprocessWithModel")}>
              <p className="sheet-action-label">{t("document.reprocessWithModel")}</p>
              {allowedModels.map((model) => (
                <button
                  key={model}
                  type="button"
                  className="sheet-action sheet-action--sub"
                  disabled={busy || isProcessing}
                  onClick={() => reprocessDocument(true, model)}
                >
                  <RefreshCw aria-hidden="true" /><span>{model}</span>
                </button>
              ))}
            </div>
          ) : null}
          <button type="button" className="sheet-action sheet-action--danger" disabled={busy} onClick={() => trash()}><Trash2 aria-hidden="true" />{t("document.trash")}</button>
        </div>
      </Sheet>
      <ShareSheet documentId={id} title={title} open={shareOpen} onClose={() => setShareOpen(false)} />
    </div>
  );
}
