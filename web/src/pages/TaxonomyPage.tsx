import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useBlocker } from "@tanstack/react-router";
import { ArrowLeft, Building2, ChevronRight, FileText, LoaderCircle, Plus, Tags, Trash2, UsersRound, X } from "lucide-react";
import { useCallback, useEffect, useRef, useState, type FormEvent, type KeyboardEvent } from "react";
import { useTranslation } from "react-i18next";

import { ApiError, api, type CorrespondentView, type PersonView, type TagView } from "../api";
import { useConfirm } from "../components/controls/ConfirmDialog";
import { SelectDropdown } from "../components/controls/SelectDropdown";
import { toast } from "../components/controls/toast";
import { toggleQueryFilter } from "../search";
import { invalidateArchive, taxonomyQuery } from "../queries";
import { ScreenLink, hrefOf } from "../shell/navigation";
import { TopBar } from "../shell/PageHead";

type Section = "people" | "correspondents" | "tags";
type TaxonomyItem = PersonView | CorrespondentView | TagView;
type RunAction = (action: () => Promise<string | void>) => Promise<void>;
type DirtyChange = (dirty: boolean) => void;

const sections = [
  { id: "people", icon: UsersRound },
  { id: "correspondents", icon: Building2 },
  { id: "tags", icon: Tags },
] as const;

/** Opens the archive filtered to one record; renders nothing when the record has no documents. */
function DocumentsLink({ prefix, name, count }: { prefix: "person" | "from" | "tag"; name: string; count: number }) {
  const { t } = useTranslation();
  if (count <= 0) return null;
  const query = toggleQueryFilter("", prefix, name);
  return (
    <ScreenLink className="button button--secondary taxonomy-documents-link" to={`${hrefOf("archive")}?${new URLSearchParams({ q: query })}`} mode="tab">
      <FileText />{t("taxonomy.showDocuments", { count })}
    </ScreenLink>
  );
}

function EntityIdentity({ prefix, name, count }: { prefix: "person" | "from" | "tag"; name: string; count: number }) {
  const { t } = useTranslation();
  return (
    <div className="taxonomy-identity">
      <div><h2>{name}</h2><p>{t("taxonomy.usedBy", { count })}</p></div>
      <DocumentsLink prefix={prefix} name={name} count={count} />
    </div>
  );
}

function NamedEditor({ kind, item, allItems, busy, onRun, onDirtyChange }: {
  kind: "people" | "correspondents";
  item: PersonView | CorrespondentView;
  allItems: Array<PersonView | CorrespondentView>;
  busy: boolean;
  onRun: RunAction;
  onDirtyChange: DirtyChange;
}) {
  const { t } = useTranslation();
  const [name, setName] = useState(item.name);
  const [notes, setNotes] = useState("notes" in item ? item.notes : "");
  const [alias, setAlias] = useState("");
  const [mergeFrom, setMergeFrom] = useState("");
  const [prevItem, setPrevItem] = useState(item);
  const { confirm, dialogElement } = useConfirm();

  const duplicates = allItems.filter((candidate) => candidate.id !== item.id);
  const dirty = name !== item.name || (kind === "people" && notes !== ("notes" in item ? item.notes : ""));

  useEffect(() => onDirtyChange(dirty), [dirty, onDirtyChange]);

  if (prevItem !== item) {
    setPrevItem(item);
    setName(item.name);
    setNotes("notes" in item ? item.notes : "");
    setAlias("");
    setMergeFrom("");
  }

  function save(event: FormEvent) {
    event.preventDefault();
    void onRun(async () => {
      if (kind === "people") await api.updatePerson(item.id, { name, notes });
      else await api.updateCorrespondent(item.id, { name });
      toast(t("taxonomy.savedToast"), { icon: "check" });
    });
  }

  function addAlias(event: FormEvent) {
    event.preventDefault();
    const value = alias.trim();
    if (!value) return;
    void onRun(async () => {
      if (kind === "people") await api.addPersonAlias(item.id, value);
      else await api.addCorrespondentAlias(item.id, value);
      setAlias("");
    });
  }

  function removeAlias(value: string) {
    void onRun(async () => {
      if (kind === "people") await api.removePersonAlias(item.id, value);
      else await api.removeCorrespondentAlias(item.id, value);
    });
  }

  async function merge() {
    const source = duplicates.find((candidate) => candidate.id === mergeFrom);
    if (!source) return;
    const ok = await confirm({
      title: t("taxonomy.mergeTitle"),
      description: t("taxonomy.mergeConfirm", { source: source.name, target: item.name }),
      confirmLabel: t("taxonomy.mergeAction"),
      destructive: false,
    });
    if (!ok) return;

    const sourceId = mergeFrom;
    setMergeFrom("");
    void onRun(async () => {
      if (kind === "people") await api.mergePeople(item.id, sourceId);
      else await api.mergeCorrespondents(item.id, sourceId);
      toast(t("taxonomy.mergedToast"), { icon: "check" });
      return item.id;
    });
  }

  async function remove() {
    const isPerson = kind === "people";
    const ok = await confirm({
      title: isPerson ? t("taxonomy.deletePerson") : t("taxonomy.deleteCorrespondent"),
      description: isPerson ? t("taxonomy.deletePersonConfirm", { name: item.name }) : t("taxonomy.deleteCorrespondentConfirm", { name: item.name }),
      confirmLabel: t("taxonomy.delete"),
      destructive: true,
    });
    if (!ok) return;

    void onRun(async () => {
      if (isPerson) await api.deletePerson(item.id);
      else await api.deleteCorrespondent(item.id);
      toast(isPerson ? t("taxonomy.personDeletedToast") : t("taxonomy.correspondentDeletedToast"), { icon: "check" });
    });
  }

  return (
    <div className="taxonomy-editor fade-in">
      <EntityIdentity prefix={kind === "people" ? "person" : "from"} name={item.name} count={item.document_count} />
      <form className="taxonomy-properties" onSubmit={save}>
        <label className="field-label">
          <span>{kind === "people" ? t("taxonomy.personName") : t("taxonomy.name")}</span>
          <input required maxLength={200} value={name} onChange={(event) => setName(event.target.value)} aria-label={t("taxonomy.nameFor", { name: item.name })} />
        </label>
        {kind === "people" ? (
          <label className="field-label">
            <span>{t("taxonomy.notes")}</span>
            <textarea rows={3} value={notes} onChange={(event) => setNotes(event.target.value)} aria-label={t("taxonomy.notesFor", { name: item.name })} />
          </label>
        ) : null}
        <div className="taxonomy-actions">
          <button className="button button--primary taxonomy-save" type="submit" disabled={busy || !dirty} aria-label={t("taxonomy.saveFor", { name: item.name })}>{t("taxonomy.save")}</button>
          {dirty ? <span className="taxonomy-dirty" role="status">{t("taxonomy.unsaved")}</span> : null}
        </div>
      </form>

      <section className="taxonomy-secondary" aria-labelledby={`aliases-title-${item.id}`}>
        <h3 id={`aliases-title-${item.id}`} className="taxonomy-secondary-title">{t("taxonomy.aliases")}</h3>
        <div className="taxonomy-secondary-body">
          {item.aliases.length > 0 ? (
            <ul className="alias-list">
              {item.aliases.map((value) => (
                <li key={value}><span>{value}</span><button type="button" className="icon-button" disabled={busy} onClick={() => removeAlias(value)} aria-label={t("taxonomy.removeAlias", { alias: value, name: item.name })}><X /></button></li>
              ))}
            </ul>
          ) : <p className="taxonomy-empty-inline">{t("taxonomy.noAliases")}</p>}
          <form className="alias-form" onSubmit={addAlias}>
            <label className="sr-only" htmlFor={`alias-${item.id}`}>{t("taxonomy.newAliasFor", { name: item.name })}</label>
            <input id={`alias-${item.id}`} value={alias} onChange={(event) => setAlias(event.target.value)} placeholder={t("taxonomy.aliasPlaceholder")} aria-label={t("taxonomy.newAliasFor", { name: item.name })} />
            <button type="submit" className="button button--secondary" disabled={busy || !alias.trim()} aria-label={t("taxonomy.addAliasFor", { name: item.name })}><Plus />{t("taxonomy.addAlias")}</button>
          </form>
        </div>
      </section>

      {duplicates.length > 0 ? (
        <section className="taxonomy-secondary taxonomy-merge" aria-labelledby={`merge-title-${item.id}`}>
          <h3 id={`merge-title-${item.id}`} className="taxonomy-secondary-title">{t("taxonomy.mergeTitle")}</h3>
          <div className="taxonomy-secondary-body">
            <div className="merge-form">
              <SelectDropdown
                id={`merge-${item.id}`}
                value={mergeFrom}
                onChange={setMergeFrom}
                options={duplicates.map((candidate) => ({ value: candidate.id, label: candidate.name }))}
                placeholder={t("taxonomy.mergePlaceholder")}
                aria-label={t("taxonomy.duplicateFor", { name: item.name })}
                disabled={busy}
              />
              <button type="button" className="button button--secondary" disabled={busy || !mergeFrom} onClick={() => void merge()} aria-label={t("taxonomy.mergeFor", { name: item.name })}>{t("taxonomy.mergeAction")}</button>
            </div>
          </div>
        </section>
      ) : null}

      <div className="taxonomy-danger-zone">
        <button type="button" className="quiet-button taxonomy-delete" disabled={busy} onClick={() => void remove()} aria-label={t("taxonomy.deleteFor", { name: item.name })}><Trash2 aria-hidden="true" />{kind === "people" ? t("taxonomy.deletePerson") : t("taxonomy.deleteCorrespondent")}</button>
      </div>

      {dialogElement}
    </div>
  );
}

function TagEditor({ item, busy, onRun, onDirtyChange }: { item: TagView; busy: boolean; onRun: RunAction; onDirtyChange: DirtyChange }) {
  const { t } = useTranslation();
  const [name, setName] = useState(item.name);
  const { confirm, dialogElement } = useConfirm();
  const dirty = name !== item.name;

  useEffect(() => onDirtyChange(dirty), [dirty, onDirtyChange]);

  function save(event: FormEvent) {
    event.preventDefault();
    void onRun(async () => {
      await api.renameTag(item.id, name);
      toast(t("taxonomy.savedToast"), { icon: "check" });
    });
  }

  async function remove() {
    const ok = await confirm({
      title: t("taxonomy.deleteTag"),
      description: t("taxonomy.deleteTagConfirm", { name: item.name, count: item.document_count }),
      confirmLabel: t("taxonomy.delete"),
      destructive: true,
    });
    if (!ok) return;

    void onRun(async () => {
      await api.deleteTag(item.id);
      toast(t("taxonomy.tagDeletedToast"), { icon: "check" });
    });
  }

  return (
    <div className="taxonomy-editor fade-in">
      <EntityIdentity prefix="tag" name={item.name} count={item.document_count} />
      <form className="taxonomy-properties" onSubmit={save}>
        <label className="field-label">
          <span>{t("taxonomy.name")}</span>
          <input required maxLength={100} value={name} onChange={(event) => setName(event.target.value)} aria-label={t("taxonomy.nameFor", { name: item.name })} />
        </label>
        <div className="tag-color-line"><span className="tag-color-swatch" style={{ background: item.color ?? "var(--line-strong)" }} /><span>{item.color ?? t("taxonomy.noColor")}</span></div>
        <div className="taxonomy-actions">
          <button className="button button--primary taxonomy-save" type="submit" disabled={busy || !dirty} aria-label={t("taxonomy.saveFor", { name: item.name })}>{t("taxonomy.save")}</button>
          {dirty ? <span className="taxonomy-dirty" role="status">{t("taxonomy.unsaved")}</span> : null}
        </div>
      </form>
      <div className="taxonomy-danger-zone">
        <button type="button" className="quiet-button taxonomy-delete" disabled={busy} onClick={() => void remove()} aria-label={t("taxonomy.deleteFor", { name: item.name })}><Trash2 aria-hidden="true" />{t("taxonomy.deleteTag")}</button>
      </div>
      {dialogElement}
    </div>
  );
}

function CreateRecord({ section, busy, onRun, onCancel, onDirtyChange }: { section: Section; busy: boolean; onRun: RunAction; onCancel: () => void; onDirtyChange: DirtyChange }) {
  const { t } = useTranslation();
  const [name, setName] = useState("");
  const [color, setColor] = useState("#63765a");
  const dirty = name !== "" || (section === "tags" && color !== "#63765a");

  useEffect(() => onDirtyChange(dirty), [dirty, onDirtyChange]);

  function create(event: FormEvent) {
    event.preventDefault();
    void onRun(async () => {
      if (section === "people") return (await api.createPerson({ name })).id;
      if (section === "correspondents") return (await api.createCorrespondent({ name })).id;
      return (await api.createTag({ name, color })).id;
    });
  }

  return (
    <form className="taxonomy-create panel-in" onSubmit={create}>
      <h2>{t(`taxonomy.createTitle.${section}`)}</h2>
      <div className="taxonomy-create-fields">
        <label className="field-label"><span>{section === "people" ? t("taxonomy.personName") : t("taxonomy.name")}</span><input required value={name} onChange={(event) => setName(event.target.value)} aria-label={t(`taxonomy.newName.${section}`)} /></label>
        {section === "tags" ? <label className="field-label tag-color-input"><span>{t("taxonomy.color")}</span><input type="color" value={color} onChange={(event) => setColor(event.target.value)} aria-label={t("taxonomy.newTagColor")} /></label> : null}
      </div>
      <div className="taxonomy-create-actions">
        <button type="submit" className="button button--primary" disabled={busy}>{t(`taxonomy.createSubmit.${section}`)}</button>
        <button type="button" className="button button--secondary" onClick={onCancel}>{t("common.cancel")}</button>
        {dirty ? <span className="taxonomy-dirty" role="status">{t("taxonomy.unsaved")}</span> : null}
      </div>
    </form>
  );
}

export function TaxonomyPage() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const { data: taxonomy, error: queryError, refetch } = useQuery(taxonomyQuery);
  const [section, setSection] = useState<Section>("people");
  const [selection, setSelection] = useState<Partial<Record<Section, string>>>({});
  const [creating, setCreating] = useState(false);
  const [detailOpen, setDetailOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const dirtyRef = useRef(false);
  const { confirm, dialogElement } = useConfirm();

  const onDirtyChange = useCallback((dirty: boolean) => {
    dirtyRef.current = dirty;
  }, []);
  const confirmDiscard = useCallback(async () => {
    if (!dirtyRef.current) return true;
    return confirm({
      title: t("taxonomy.unsavedTitle"),
      description: t("taxonomy.unsavedDescription"),
      confirmLabel: t("taxonomy.discardChanges"),
      cancelLabel: t("taxonomy.keepEditing"),
      destructive: false,
    });
  }, [confirm, t]);
  const shouldBlock = useCallback(async () => !(await confirmDiscard()), [confirmDiscard]);
  const enableBeforeUnload = useCallback(() => dirtyRef.current, []);
  useBlocker({ shouldBlockFn: shouldBlock, enableBeforeUnload });

  const error = actionError ?? (queryError instanceof ApiError ? queryError.code : queryError ? "network" : null);
  const items = (taxonomy?.[section] ?? []) as TaxonomyItem[];
  const selected = items.find((item) => item.id === selection[section]) ?? items[0];
  const hasDetail = creating || Boolean(selected);

  useEffect(() => {
    if (!creating && !selected) dirtyRef.current = false;
  }, [creating, selected]);

  async function transition(action: () => void): Promise<boolean> {
    if (busy || !(await confirmDiscard())) return false;
    dirtyRef.current = false;
    action();
    return true;
  }

  /** Arrow keys move the highlighted record through the list; the first press highlights the first record. */
  async function onListKeyDown(event: KeyboardEvent<HTMLUListElement>) {
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    event.preventDefault();
    const list = event.currentTarget;
    const current = items.findIndex((item) => item.id === selection[section]);
    const next = Math.min(items.length - 1, Math.max(0, current + (event.key === "ArrowDown" ? 1 : -1)));
    const target = items[next];
    if (!target) return;
    const moved = await transition(() => {
      setSelection((state) => ({ ...state, [section]: target.id }));
      setCreating(false);
      setActionError(null);
    });
    if (moved) list.querySelectorAll<HTMLButtonElement>("button")[next]?.focus();
  }

  async function run(action: () => Promise<string | void>) {
    const wasCreating = creating;
    setBusy(true);
    setActionError(null);
    try {
      const nextSelection = await action();
      await invalidateArchive(queryClient);
      if (nextSelection) {
        setSelection((current) => ({ ...current, [section]: nextSelection }));
        setDetailOpen(true);
      }
      if (wasCreating) dirtyRef.current = false;
      setCreating(false);
    } catch (cause) {
      setActionError(cause instanceof ApiError ? cause.code : "network");
    } finally {
      setBusy(false);
    }
  }

  const workspaceClass = [
    "taxonomy-workspace",
    hasDetail ? "taxonomy-workspace--has-detail" : "",
    hasDetail && detailOpen ? "taxonomy-workspace--detail-open" : "",
  ].filter(Boolean).join(" ");

  return (
    <div className="taxonomy-page">
      <TopBar title={t("taxonomy.title")} />
      <header className="taxonomy-header">
        <div><h1>{t("taxonomy.title")}</h1><p>{t("taxonomy.intro")}</p></div>
        <button type="button" className="button button--primary" disabled={!taxonomy || busy} onClick={() => void transition(() => { setCreating((value) => !value); setDetailOpen(true); })}><Plus />{t(`taxonomy.add.${section}`)}</button>
      </header>

      <div className="taxonomy-tabs" role="tablist" aria-label={t("taxonomy.sections")}>
        {sections.map(({ id, icon: Icon }) => <button key={id} type="button" role="tab" aria-label={t(`taxonomy.tabs.${id}`)} aria-selected={section === id} className={section === id ? "taxonomy-tab taxonomy-tab--active" : "taxonomy-tab"} onClick={() => void transition(() => { setSection(id); setCreating(false); setDetailOpen(false); setActionError(null); })}><Icon />{t(`taxonomy.tabs.${id}`)}<span>{taxonomy?.[id].length ?? 0}</span></button>)}
      </div>

      {error ? <div className="taxonomy-error" role="alert"><span>{t(`errors.${error}`, { defaultValue: t("errors.taxonomy") })}</span>{!taxonomy ? <button type="button" className="quiet-button" onClick={() => { setActionError(null); void refetch(); }}>{t("common.retry")}</button> : null}</div> : null}
      {busy ? <div className="taxonomy-saving" role="status"><LoaderCircle className="spin" />{t("taxonomy.saving")}</div> : null}

      {!taxonomy && !error ? <div className="taxonomy-loading"><LoaderCircle className="spin" /><p>{t("taxonomy.loading")}</p></div> : null}
      {taxonomy ? (
        <div key={section} className={`${workspaceClass} panel-in`}>
          <section className="taxonomy-list" aria-label={t(`taxonomy.tabs.${section}`)}>
            {items.length > 0 ? <ul onKeyDown={(event) => void onListKeyDown(event)}>{items.map((item) => <li key={item.id} className={selection[section] === item.id && !creating ? "taxonomy-list-item taxonomy-list-item--active" : "taxonomy-list-item"}><button type="button" onClick={() => void transition(() => { setSelection((current) => ({ ...current, [section]: item.id })); setCreating(false); setDetailOpen(true); setActionError(null); })}>{item.name}<ChevronRight /></button><span>{t("taxonomy.usedBy", { count: item.document_count })}</span></li>)}</ul> : <div className="taxonomy-empty"><p>{t(`taxonomy.empty.${section}`)}</p></div>}
          </section>
          {hasDetail ? (
            <section className="taxonomy-detail" aria-label={t("taxonomy.recordDetails")}>
              <button type="button" className="quiet-button taxonomy-back" onClick={() => void transition(() => { setCreating(false); setDetailOpen(false); })}><ArrowLeft />{t("taxonomy.backToList")}</button>
              {creating ? <CreateRecord key={section} section={section} busy={busy} onRun={run} onCancel={() => void transition(() => { setCreating(false); setDetailOpen(Boolean(selected)); })} onDirtyChange={onDirtyChange} /> : null}
              {!creating && selected && section === "people" ? <NamedEditor key={selected.id} kind="people" item={selected as PersonView} allItems={taxonomy.people} busy={busy} onRun={run} onDirtyChange={onDirtyChange} /> : null}
              {!creating && selected && section === "correspondents" ? <NamedEditor key={selected.id} kind="correspondents" item={selected as CorrespondentView} allItems={taxonomy.correspondents} busy={busy} onRun={run} onDirtyChange={onDirtyChange} /> : null}
              {!creating && selected && section === "tags" ? <TagEditor key={selected.id} item={selected as TagView} busy={busy} onRun={run} onDirtyChange={onDirtyChange} /> : null}
            </section>
          ) : null}
        </div>
      ) : null}
      {dialogElement}
    </div>
  );
}
