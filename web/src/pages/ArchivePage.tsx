import { useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertCircle, ArrowDown, CalendarClock, CheckSquare, ChevronRight, FileText, LoaderCircle, RefreshCw, Search, SlidersHorizontal, Trash2, X } from "lucide-react";
import { memo, useCallback, useEffect, useMemo, useRef, useState, type FormEvent, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { useTranslation } from "react-i18next";

import { ApiError, api, type FieldValue, type Hit, type TypeView } from "../api";
import { SelectDropdown } from "../components/controls/SelectDropdown";
import { toast } from "../components/controls/toast";
import { usePrefetchVisible } from "../lib/prefetchVisible";
import { documentQuery, invalidateArchive, searchQuery, taxonomyQuery } from "../queries";
import { dueState, formatIsoDate } from "../format";
import { archiveSearchOf, calmSnippet, hasFilterTokens, hasQueryToken, parseSnippet, queryFilter, toggleQueryFilter } from "../search";
import { documentPath } from "../shell/nav";
import { ScreenLink, hrefOf, useGo } from "../shell/navigation";
import { TopBar } from "../shell/PageHead";
import { useScreen } from "../shell/ScreenFrame";
import { Sheet } from "../shell/Sheet";

type Sort = "relevance" | "date_desc" | "date_asc";
/** People shown on a result before the rest collapse into a count. */
const PEOPLE_SHOWN = 2;
/** Organizations shown on a result before the rest collapse into a count. */
const ORGANIZATIONS_SHOWN = 2;

function ValueView({ value, language }: { value: FieldValue; language: string }) {
  if (value.kind === "money") {
    return <>{new Intl.NumberFormat(language, { style: "currency", currency: value.value.currency }).format(Number(value.value.amount))}</>;
  }
  if (value.kind === "list") return <>{value.value.join(", ")}</>;
  if (value.kind === "date") return <>{formatIsoDate(value.value, language)}</>;
  return <>{value.value}</>;
}

function Snippet({ text, language }: { text: string; language: string }) {
  return <>{calmSnippet(parseSnippet(text), language).map((part, index) => part.mark ? <mark key={index}>{part.text}</mark> : <span key={index}>{part.text}</span>)}</>;
}

/** True when the browser would draw a focus ring: focus from the keyboard, not from a tap. */
function focusVisible(element: Element): boolean {
  try { return element.matches(":focus-visible"); } catch { return false; }
}

/** A click on these belongs to them, not to the card around them. */
const INTERACTIVE = "a, button, input, label, select, textarea, summary, [role=button]";

/** Every prop is a primitive or referentially stable across the page's re-renders, so a filter
 *  tap or a selection toggle re-renders only the rows whose own data changed. */
const ResultItem = memo(function ResultItem({ hit, type, active, index, language, onActivate, onFilter, resultRef, checked, onToggle }: {
  hit: Hit;
  type?: TypeView;
  active: boolean;
  index: number;
  language: string;
  /** Marks the row the keyboard roves from; `highlight` shows or hides its highlight, undefined leaves it as is. */
  onActivate: (index: number, highlight?: boolean) => void;
  onFilter: (prefix: string, value?: string) => void;
  resultRef: (index: number, element: HTMLAnchorElement | null) => void;
  /** Whether the row is ticked; meaningful only while `onToggle` is set. */
  checked: boolean;
  /** Present while the archive is in selection mode. */
  onToggle?: (id: string, checked: boolean) => void;
}) {
  const { t } = useTranslation();
  const linkRef = useCallback((element: HTMLAnchorElement | null) => resultRef(index, element), [resultRef, index]);
  const typeLabel = type?.labels[language] ?? type?.labels.en ?? hit.type_key ?? t("archive.untyped");
  // The due date shows beside the key fields, unless one of them already prints that very date.
  const due = hit.due_date && !Object.values(hit.key_fields).some((value) => value.kind === "date" && value.value === hit.due_date) ? dueState(hit.due_date) : null;
  return (
    <li data-prefetch-id={hit.id}>
      <article
        className={active ? "result-row result-row--active" : "result-row"}
        onPointerEnter={(event) => { if (event.pointerType === "mouse") onActivate(index, true); }}
        onPointerDown={(event) => { if (event.pointerType !== "mouse") onActivate(index, false); }}
        // The whole card opens the document (or ticks the row); text selection, links and controls keep their own clicks.
        onClick={(event) => {
          if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
          if ((event.target as Element).closest(INTERACTIVE)) return;
          if (window.getSelection()?.toString()) return;
          if (onToggle) onToggle(hit.id, !checked);
          else event.currentTarget.querySelector<HTMLAnchorElement>("a.result-title")?.click();
        }}
      >
        {onToggle ? (
          <label className="result-select">
            <input type="checkbox" checked={checked} onChange={(event) => onToggle(hit.id, event.target.checked)} aria-label={t("archive.selectDocument", { title: hit.title })} />
          </label>
        ) : null}
        <ScreenLink to={documentPath(hit.id)} sharedSelector="[data-shared-source]" className="result-thumbnail" tabIndex={-1} aria-hidden="true">
          <span className="result-thumbnail-frame" data-shared-source>
            <FileText />
            {hit.has_pages === false ? null : (
              <img
                src={api.thumbnailUrl(hit.id)}
                alt=""
                loading={index < 8 ? "eager" : "lazy"}
                decoding="async"
                ref={(node) => { if (node?.complete && node.naturalWidth > 0) node.classList.add("is-loaded"); }}
                onLoad={(event) => event.currentTarget.classList.add("is-loaded")}
                onError={(event) => { event.currentTarget.hidden = true; }}
              />
            )}
          </span>
        </ScreenLink>
        <div className="result-body">
          <div className="result-heading">
            <ScreenLink ref={linkRef} to={documentPath(hit.id)} sharedSelector="[data-shared-source]" className="result-title" onFocus={(event) => onActivate(index, focusVisible(event.currentTarget) ? true : undefined)}>{hit.title}</ScreenLink>
            {hit.document_date ? <time dateTime={hit.document_date}>{formatIsoDate(hit.document_date, language)}</time> : null}
            {(hit.attention ?? []).length > 0 ? <span className="result-attention"><AlertCircle aria-hidden="true" />{t("archive.needsAttention")}</span> : null}
          </div>
          <div className="result-taxonomy">
            <button type="button" className="text-chip" onClick={() => onFilter("type", hit.type_key ?? "")} disabled={!hit.type_key}>{typeLabel}</button>
            {(hit.organizations ?? []).slice(0, ORGANIZATIONS_SHOWN).map((org) => (
              <button type="button" className="text-chip text-chip--plain" key={org} onClick={() => onFilter("from", org)}>{org}</button>
            ))}
            {(hit.organizations ?? []).length > ORGANIZATIONS_SHOWN ? (
              <span className="text-chip-more" title={(hit.organizations ?? []).slice(ORGANIZATIONS_SHOWN).join(", ")}>+{(hit.organizations ?? []).length - ORGANIZATIONS_SHOWN}</span>
            ) : null}
            {hit.people.slice(0, PEOPLE_SHOWN).map((person) => <button type="button" className="text-chip text-chip--plain" key={person} onClick={() => onFilter("person", person)}>{person}</button>)}
            {hit.people.length > PEOPLE_SHOWN ? <span className="text-chip-more" title={hit.people.slice(PEOPLE_SHOWN).join(", ")}>+{hit.people.length - PEOPLE_SHOWN}</span> : null}
          </div>
          {Object.keys(hit.key_fields).length > 0 || due ? (
            <dl className="result-fields">
              {Object.entries(hit.key_fields).slice(0, 3).map(([key, value]) => (
                <div key={key}>
                  <dt>{type?.fields.find((field) => field.key === key)?.labels[language] ?? key.replaceAll("_", " ")}</dt>
                  <dd><ValueView value={value} language={language} /></dd>
                </div>
              ))}
              {due ? (
                <div className={due.state === "later" ? "result-due" : "result-due result-due--soon"}>
                  <dt>{t("archive.due")}</dt>
                  <dd><time dateTime={hit.due_date ?? undefined}>{formatIsoDate(hit.due_date ?? "", language)}</time></dd>
                </div>
              ) : null}
            </dl>
          ) : null}
          {hit.snippet.trim() !== "" ? <p className="result-snippet"><Snippet text={hit.snippet} language={language} /></p> : null}
        </div>
        <ScreenLink to={documentPath(hit.id)} sharedSelector="[data-shared-source]" className="result-open" tabIndex={-1} aria-label={t("archive.open", { title: hit.title })}><ChevronRight /></ScreenLink>
      </article>
    </li>
  );
});

export function ArchivePage() {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();
  const go = useGo();
  // The search belongs to this screen's entry in the stack, not to the address bar: the archive
  // kept under an open document keeps its query while the address is the document's.
  const { search, active } = useScreen();
  const { query, sort } = archiveSearchOf(search);
  const { data: taxonomy } = useQuery(taxonomyQuery);

  const { data: page, isLoading, isPlaceholderData, error: queryError } = useQuery(searchQuery(query, sort));
  // How many documents are due soon or past due: the quick filter is offered only when there are any.
  const { data: expiringPage } = useQuery(searchQuery("has:expiring", "relevance"));

  const setSearchParams = useCallback((params: Record<string, string>) => {
    const text = new URLSearchParams(params).toString();
    go(`${hrefOf("archive")}${text === "" ? "" : `?${text}`}`, { mode: "replace", replace: true });
  }, [go]);

  const [extraHits, setExtraHits] = useState<Hit[]>([]);
  const [extraCursor, setExtraCursor] = useState<string | null | undefined>(undefined);
  const [loadingMore, setLoadingMore] = useState(false);
  const [loadMoreError, setLoadMoreError] = useState(false);
  const [prevQueryKey, setPrevQueryKey] = useState(`${query}\0${sort}`);
  const [activeIndex, setActiveIndex] = useState(-1);
  // The active row is drawn only after the keyboard or a mouse put it there, never on a tap or a fresh list.
  const [highlight, setHighlight] = useState(false);
  const [prevPage, setPrevPage] = useState(page);
  const [filtersOpen, setFiltersOpen] = useState(false);
  const [selecting, setSelecting] = useState(false);
  const [selected, setSelected] = useState<ReadonlySet<string>>(() => new Set());
  const [bulk, setBulk] = useState<{ done: number; total: number } | null>(null);

  if (prevQueryKey !== `${query}\0${sort}`) {
    setPrevQueryKey(`${query}\0${sort}`);
    setExtraHits([]);
    setExtraCursor(undefined);
    setLoadMoreError(false);
    setSelected(new Set());
  }

  // Documents added elsewhere (another device, an agent) bring types the cached taxonomy has not
  // counted yet, so their quick filters would be missing. A hit of an unknown type refreshes it,
  // once per set of unknown types.
  const unknownTypesChecked = useRef("");
  useEffect(() => {
    if (!page || !taxonomy) return;
    const counted = new Set(taxonomy.types.filter((type) => type.document_count > 0).map((type) => type.key));
    const unknown = [...new Set(page.hits.map((hit) => hit.type_key).filter((key): key is string => Boolean(key) && !counted.has(key as string)))].sort().join(",");
    if (unknown === "" || unknown === unknownTypesChecked.current) return;
    unknownTypesChecked.current = unknown;
    void queryClient.invalidateQueries({ queryKey: ["taxonomy"] });
  }, [page, taxonomy, queryClient]);

  if (prevPage !== page) {
    setPrevPage(page);
    setActiveIndex(-1);
  }
  const activate = useCallback((index: number, show?: boolean) => {
    // A finger only hides the highlight; already hidden, the tap redraws nothing before its click.
    if (show === false) {
      setHighlight(false);
      return;
    }
    setActiveIndex(index);
    if (show !== undefined) setHighlight(show);
  }, []);

  const hits = useMemo(() => (page ? [...page.hits, ...extraHits] : []), [page, extraHits]);
  const total = page?.total ?? 0;
  const nextCursor = extraCursor !== undefined ? extraCursor : page?.next_cursor ?? null;
  const loading = isLoading && !page;
  const error = queryError instanceof ApiError ? queryError.code : queryError ? "network" : null;
  // Nothing in the archive at all: no search or toolbar to offer, only the empty state.
  const archiveEmpty = !loading && !error && !query && total === 0;
  // Once the list is empty (everything in it was trashed) there is nothing left to select.
  if (selecting && !loading && hits.length === 0) {
    setSelecting(false);
    setSelected(new Set());
  }
  const searchRef = useRef<HTMLInputElement>(null);
  const pendingExample = useRef<string | null>(null);
  const resultRefs = useRef<(HTMLAnchorElement | null)[]>([]);
  const setResultRef = useCallback((index: number, element: HTMLAnchorElement | null) => { resultRefs.current[index] = element; }, []);
  const typeByKey = useMemo(() => new Map((taxonomy?.types ?? []).map((type) => [type.key, type])), [taxonomy]);

  // On a phone there is no hover to warm a card before the tap: fetch the text of the ones in view.
  const resultsRef = useRef<HTMLOListElement>(null);
  const prefetchDocument = useCallback((id: string) => { void queryClient.prefetchQuery(documentQuery(id, 1)); }, [queryClient]);
  usePrefetchVisible(resultsRef, active, hits, prefetchDocument);

  useEffect(() => {
    if (!active) return;
    function onKeyDown(event: KeyboardEvent) {
      const target = event.target as HTMLElement | null;
      const editing = target?.matches("input, textarea, select, [contenteditable=true]") ?? false;
      if (event.key === "/" && !editing) {
        event.preventDefault();
        searchRef.current?.focus();
        return;
      }
      if (event.key === "Escape" && !editing && selecting) {
        setSelecting(false);
        setSelected(new Set());
        return;
      }
      if (event.key === "Escape" && !editing) {
        if (query) {
          const params: Record<string, string> = {};
          if (sort !== "relevance") params.sort = sort;
          setSearchParams(params);
        }
        return;
      }
      if ((event.key === "ArrowDown" || event.key === "ArrowUp") && hits.length > 0 && (!editing || target === searchRef.current)) {
        event.preventDefault();
        const direction = event.key === "ArrowDown" ? 1 : -1;
        const next = Math.min(hits.length - 1, Math.max(0, activeIndex + direction));
        setActiveIndex(next);
        setHighlight(true);
        resultRefs.current[next]?.focus();
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [active, activeIndex, hits, query, selecting, setSearchParams, sort]);

  // The result rows are memoized, so what they call must not change with the query: the handlers
  // read the current query and sort from here instead of closing over them.
  const latest = useRef({ query, sort });
  useEffect(() => {
    latest.current = { query, sort };
  });

  const changeQuery = useCallback((next: string) => {
    const params: Record<string, string> = {};
    if (next) params.q = next;
    if (latest.current.sort !== "relevance") params.sort = latest.current.sort;
    setSearchParams(params);
  }, [setSearchParams]);

  function submit(event: FormEvent) {
    event.preventDefault();
    changeQuery(searchRef.current?.value.trim() ?? "");
  }

  const filter = useCallback((prefix: string, value?: string) => {
    changeQuery(toggleQueryFilter(latest.current.query, prefix, value));
    // A fine pointer means a hardware keyboard to refine the query with; on a phone focus would only raise the on-screen keyboard.
    if (window.matchMedia?.("(pointer: fine)").matches) searchRef.current?.focus();
  }, [changeQuery]);

  function changeSort(nextSort: Sort) {
    const params: Record<string, string> = {};
    if (query) params.q = query;
    if (nextSort !== "relevance") params.sort = nextSort;
    setSearchParams(params);
  }

  function onInputKeyDown(event: ReactKeyboardEvent<HTMLInputElement>) {
    if (event.key === "Escape" && event.currentTarget.value) {
      event.stopPropagation();
      event.currentTarget.value = "";
      changeQuery("");
    }
  }

  // Picking an example closes the modal filters sheet first; once Sheet's inert/focus cleanup has
  // completed in the same commit, the effect below drops the example into the field and focuses it.
  function pickExample(example: string) {
    pendingExample.current = example;
    setFiltersOpen(false);
  }

  useEffect(() => {
    if (filtersOpen || pendingExample.current === null) return;
    const example = pendingExample.current;
    pendingExample.current = null;
    const input = searchRef.current;
    if (!input) return;
    const current = input.value.trim();
    input.value = [current, example].filter(Boolean).join(" ");
    input.focus();
    input.setSelectionRange?.(input.value.length, input.value.length);
  }, [filtersOpen]);

  const syntaxFilters = [
    { example: "type:passport", label: t("archive.syntaxType") },
    { example: 'person:"ivan petrov"', label: t("archive.syntaxPerson") },
    { example: "from:acme", label: t("archive.syntaxFrom") },
    { example: "tag:warranty", label: t("archive.syntaxTag") },
    { example: "has:attention", label: t("archive.syntaxHas") },
    { example: "has:expiring", label: t("archive.syntaxExpiring") },
    { example: "date:>=2024-01", label: t("archive.syntaxDate") },
    { example: "due:<2027-06", label: t("archive.syntaxDue") },
    { example: "added:<2025", label: t("archive.syntaxAdded") },
    { example: "field.expiry_date<2027-01-01", label: t("archive.syntaxField") },
    { example: "local:yes", label: t("archive.syntaxLocal") },
  ];
  const syntaxCombinations = [
    { example: '"annual report"', label: t("archive.syntaxPhrase") },
    { example: "rent OR lease", label: t("archive.syntaxOr") },
    { example: "-local:yes", label: t("archive.syntaxNot") },
    { example: "(rent OR lease) deposit", label: t("archive.syntaxParens") },
  ];

  async function loadMore() {
    if (!nextCursor || loadingMore) return;
    const reqCursor = nextCursor;
    const reqQuery = query;
    const reqSort = sort;
    setLoadingMore(true);
    setLoadMoreError(false);
    try {
      const nextPage = await api.search({ query: reqQuery, sort: reqSort, cursor: reqCursor });
      if (reqQuery !== query || reqSort !== sort) return;
      setExtraHits((current) => {
        const existingIds = new Set([
          ...(page?.hits.map((hit) => hit.id) ?? []),
          ...current.map((hit) => hit.id),
        ]);
        const newHits = nextPage.hits.filter((hit) => !existingIds.has(hit.id));
        return [...current, ...newHits];
      });
      setExtraCursor(nextPage.next_cursor);
      setLoadMoreError(false);
    } catch {
      if (reqQuery !== query || reqSort !== sort) return;
      setLoadMoreError(true);
    } finally {
      setLoadingMore(false);
    }
  }

  const toggleSelected = useCallback((id: string, checked: boolean) => {
    setSelected((previous) => {
      const next = new Set(previous);
      if (checked) next.add(id);
      else next.delete(id);
      return next;
    });
  }, []);

  function stopSelecting() {
    setSelecting(false);
    setSelected(new Set());
  }

  function refreshAfterBulk() {
    void invalidateArchive(queryClient);
  }

  // One request per document, one after another: the archive is personal-sized, and a
  // sequential run keeps the progress honest and the server unhurried.
  async function runBulk(action: (id: string) => Promise<unknown>, done: (count: number) => string, undo?: (ids: string[]) => Promise<unknown>) {
    const ids = [...selected];
    if (ids.length === 0 || bulk) return;
    setBulk({ done: 0, total: ids.length });
    const succeeded: string[] = [];
    for (const [index, id] of ids.entries()) {
      try {
        await action(id);
        succeeded.push(id);
      } catch {
        // Counted as a failure in the summary below.
      }
      setBulk({ done: index + 1, total: ids.length });
    }
    setBulk(null);
    setSelected(new Set());
    refreshAfterBulk();
    const failed = ids.length - succeeded.length;
    const message = [done(succeeded.length), failed > 0 ? t("archive.bulkFailed", { count: failed }) : ""].filter(Boolean).join(" · ");
    toast(message, {
      icon: failed > 0 ? "alert" : "check",
      ...(undo && succeeded.length > 0 ? { undoLabel: t("common.undo"), undo: () => void Promise.allSettled(succeeded.map((id) => undo([id]))).then(refreshAfterBulk) } : {}),
    });
  }

  async function addTo(kind: "people" | "tags", documentId: string, valueId: string) {
    const view = await api.document(documentId);
    const ids = view[kind].map((item) => item.id);
    if (!ids.includes(valueId)) await api.patchDocument(documentId, { [kind]: [...ids, valueId] });
  }

  const allSelected = hits.length > 0 && selected.size === hits.length;
  const hasSelection = selected.size > 0 || bulk !== null;
  const attentionActive = hasQueryToken(query, "has:attention");
  const expiringActive = hasQueryToken(query, "has:expiring");
  const showExpiring = expiringActive || (expiringPage?.total ?? 0) > 0;
  const language = i18n.resolvedLanguage ?? "en";
  const usedTypes = (taxonomy?.types ?? [])
    .filter((type) => type.document_count > 0)
    .sort((a, b) => b.document_count - a.document_count);
  const typeLabelOf = (type: TypeView) => type.labels[language] ?? type.labels.en;
  const filtered = hasFilterTokens(query);
  const activeFilters = (attentionActive ? 1 : 0) + (expiringActive ? 1 : 0) + usedTypes.filter((type) => hasQueryToken(query, queryFilter("type", type.key))).length;
  const sortOptions = [
    { value: "relevance", label: t("archive.relevance") },
    { value: "date_desc", label: t("archive.newest") },
    { value: "date_asc", label: t("archive.oldest") },
  ] satisfies { value: Sort; label: string }[];

  return (
    <div className="archive-page">
      <TopBar title={t("archive.title")} />
      <header className="archive-header">
        <div>
          <h1>{t("archive.title")}</h1>
        </div>
        <ScreenLink to={hrefOf("upload")} mode="tab" className="button button--primary archive-add">{t("archive.add")}</ScreenLink>
      </header>

      {archiveEmpty ? null : <>
      <div className="search-workspace">
        <form className="search-form" role="search" onSubmit={submit}>
          <Search aria-hidden="true" />
          <input
            key={query}
            ref={searchRef}
            defaultValue={query}
            onKeyDown={onInputKeyDown}
            placeholder={t("archive.searchPlaceholder")}
            aria-label={t("archive.searchLabel")}
            autoComplete="off"
            spellCheck={false}
          />
          {query ? (
            <button
              type="button"
              className="search-clear"
              onClick={() => {
                if (searchRef.current) searchRef.current.value = "";
                changeQuery("");
              }}
              aria-label={t("archive.clear")}
            >
              <X />
            </button>
          ) : null}
          <kbd>/</kbd>
          <button type="submit" className="search-submit">{t("archive.search")}</button>
        </form>
        <div className="quick-filters" aria-label={t("archive.quickFilters")}>
          <button type="button" className={attentionActive ? "filter-chip filter-chip--active" : "filter-chip"} aria-pressed={attentionActive} onClick={() => filter("has:attention")}>
            <AlertCircle />{t("archive.attention")}
          </button>
          {showExpiring ? (
            <button type="button" className={expiringActive ? "filter-chip filter-chip--active" : "filter-chip"} aria-pressed={expiringActive} onClick={() => filter("has:expiring")}>
              <CalendarClock />{t("archive.expiring")}
            </button>
          ) : null}
          {usedTypes.slice(0, 4).map((type) => {
            const on = hasQueryToken(query, queryFilter("type", type.key));
            return (
              <button key={type.id} type="button" className={on ? "filter-chip filter-chip--active" : "filter-chip"} aria-pressed={on} onClick={() => filter("type", type.key)}>
                {typeLabelOf(type)}
              </button>
            );
          })}
        </div>
      </div>

      <div className="results-meta" aria-live="polite">
        {!loading && !error ? <span className="results-count">{t("archive.results", { count: total })}</span> : null}
        {query ? <button type="button" className="quiet-button results-reset" onClick={() => changeQuery("")}>{t("archive.allDocuments")}</button> : null}
        <div className="results-tools">
          <button type="button" className="quiet-button filters-button" aria-haspopup="dialog" aria-label={activeFilters > 0 ? t("archive.filtersActive", { count: activeFilters }) : t("archive.filters")} onClick={() => setFiltersOpen(true)}>
            <SlidersHorizontal aria-hidden="true" />{t("archive.filters")}{activeFilters > 0 ? <span className="filters-count" aria-hidden="true">{activeFilters}</span> : null}
          </button>
          {hits.length > 0 ? (
            <div className="results-sort">
              <SelectDropdown value={sort} onChange={(next) => changeSort(next as Sort)} options={sortOptions} aria-label={t("archive.sort")} />
            </div>
          ) : null}
          {hits.length > 0 && !selecting ? <button type="button" className="quiet-button results-select" onClick={() => setSelecting(true)}><CheckSquare aria-hidden="true" />{t("archive.select")}</button> : null}
        </div>
      </div>
      </>}

      <Sheet open={filtersOpen} title={t("archive.filters")} onClose={() => setFiltersOpen(false)}>
        <div className="filters-sheet">
          <section>
            <h4>{t("archive.filterShow")}</h4>
            <div className="filters-chips">
              <button type="button" className={attentionActive ? "filter-chip filter-chip--active" : "filter-chip"} aria-pressed={attentionActive} onClick={() => changeQuery(toggleQueryFilter(query, "has:attention"))}>
                <AlertCircle />{t("archive.attention")}
              </button>
              {showExpiring ? (
                <button type="button" className={expiringActive ? "filter-chip filter-chip--active" : "filter-chip"} aria-pressed={expiringActive} onClick={() => changeQuery(toggleQueryFilter(query, "has:expiring"))}>
                  <CalendarClock />{t("archive.expiring")}
                </button>
              ) : null}
            </div>
          </section>
          {usedTypes.length > 0 ? (
            <section>
              <h4>{t("archive.filterType")}</h4>
              <div className="filters-chips">
                {usedTypes.map((type) => {
                  const on = hasQueryToken(query, queryFilter("type", type.key));
                  return (
                    <button key={type.id} type="button" className={on ? "filter-chip filter-chip--active" : "filter-chip"} aria-pressed={on} onClick={() => changeQuery(toggleQueryFilter(query, "type", type.key))}>
                      {typeLabelOf(type)}
                    </button>
                  );
                })}
              </div>
            </section>
          ) : null}
          <section>
            <h4>{t("archive.sort")}</h4>
            <div className="filters-chips">
              {sortOptions.map((option) => (
                <button key={option.value} type="button" className={sort === option.value ? "filter-chip filter-chip--active" : "filter-chip"} aria-pressed={sort === option.value} onClick={() => changeSort(option.value)}>
                  {option.label}
                </button>
              ))}
            </div>
          </section>
          <section>
            <h4>{t("archive.syntax")}</h4>
            <p>{t("archive.syntaxDescription")} {t("archive.syntaxHint")}</p>
            <h5 className="syntax-group-title">{t("archive.syntaxFilters")}</h5>
            <ul className="syntax-group">
              {syntaxFilters.map((item) => (
                <li key={item.example}>
                  <button type="button" className="syntax-example" onClick={() => pickExample(item.example)}>
                    <code>{item.example}</code>
                    <span>{item.label}</span>
                  </button>
                </li>
              ))}
            </ul>
            <h5 className="syntax-group-title">{t("archive.syntaxCombinations")}</h5>
            <ul className="syntax-group">
              {syntaxCombinations.map((item) => (
                <li key={item.example}>
                  <button type="button" className="syntax-example" onClick={() => pickExample(item.example)}>
                    <code>{item.example}</code>
                    <span>{item.label}</span>
                  </button>
                </li>
              ))}
            </ul>
          </section>
          <button type="button" className="button button--primary filters-done" onClick={() => setFiltersOpen(false)}>{t("archive.filtersDone")}</button>
        </div>
      </Sheet>

      {error ? (
        <div className="inline-error" role="alert">
          <p>{t(`errors.${error}`, { defaultValue: t("errors.search") })}</p>
          <button type="button" className="button button--secondary" onClick={() => void queryClient.invalidateQueries({ queryKey: ["search"] })}>{t("common.retry")}</button>
        </div>
      ) : null}

      {loading ? (
        <div className="results-loading" aria-label={t("archive.loading")}>{[0, 1, 2].map((item) => <div className="result-skeleton" key={item} />)}</div>
      ) : null}

      {!loading && !error && hits.length === 0 ? (
        <section className="empty-results">
          <FileText />
          <h2>{query ? t("archive.noMatches") : t("archive.emptyTitle")}</h2>
          <p>{query ? t(filtered ? "archive.noMatchesDescription" : "archive.noMatchesWordsDescription") : t("archive.emptyDescription")}</p>
          {filtered ? <button type="button" className="button button--secondary" onClick={() => changeQuery("")}>{t("archive.clearFilters")}</button> : query ? null : <ScreenLink className="button button--primary" to={hrefOf("upload")} mode="tab">{t("archive.addFirst")}</ScreenLink>}
        </section>
      ) : null}

      {!loading && !error && page?.partial && hits.length > 0 ? (
        <p className="partial-note" role="status">{t("archive.partialNote")}</p>
      ) : null}

      {!loading && hits.length > 0 ? (
        <ol ref={resultsRef} className={["results-list", query ? "results-list--searching" : "", isPlaceholderData ? "results-list--stale" : ""].filter(Boolean).join(" ")} aria-busy={isPlaceholderData || undefined}>
          {hits.map((hit, index) => (
            <ResultItem
              key={hit.id}
              hit={hit}
              type={hit.type_key ? typeByKey.get(hit.type_key) : undefined}
              active={highlight && activeIndex === index}
              index={index}
              language={language}
              onActivate={activate}
              onFilter={filter}
              resultRef={setResultRef}
              checked={selecting && selected.has(hit.id)}
              onToggle={selecting ? toggleSelected : undefined}
            />
          ))}
        </ol>
      ) : null}

      {selecting ? (
        <div className={hasSelection ? "bulk-bar" : "bulk-bar bulk-bar--idle"} role="toolbar" aria-label={t("archive.bulkActions")}>
          <span className="bulk-count" aria-live="polite">
            {bulk ? <><LoaderCircle className="spin" aria-hidden="true" />{t("archive.bulkWorking", { done: bulk.done, total: bulk.total })}</> : hasSelection ? t("archive.selected", { count: selected.size }) : t("archive.selectHint")}
          </span>
          <button type="button" className="quiet-button" disabled={Boolean(bulk)} onClick={() => setSelected(allSelected ? new Set() : new Set(hits.map((hit) => hit.id)))}>
            {allSelected ? t("archive.selectNone") : t("archive.selectAll", { count: hits.length })}
          </button>
          {hasSelection ? <div className="bulk-actions">
            <button type="button" className="button button--secondary" disabled={Boolean(bulk)} onClick={() => void runBulk((id) => api.reprocess(id, true), (count) => t("archive.bulkReprocessed", { count }))}><RefreshCw aria-hidden="true" />{t("archive.bulkReprocess")}</button>
            {taxonomy && taxonomy.people.length > 0 ? (
              <SelectDropdown className="bulk-select" value="" placeholder={t("archive.bulkAddPerson")} disabled={Boolean(bulk)} onChange={(id) => { if (id) void runBulk((documentId) => addTo("people", documentId, id), (count) => t("archive.bulkUpdated", { count })); }} aria-label={t("archive.bulkAddPerson")} options={taxonomy.people.map((person) => ({ value: person.id, label: person.name }))} />
            ) : null}
            {taxonomy && taxonomy.tags.length > 0 ? (
              <SelectDropdown className="bulk-select" value="" placeholder={t("archive.bulkAddTag")} disabled={Boolean(bulk)} onChange={(id) => { if (id) void runBulk((documentId) => addTo("tags", documentId, id), (count) => t("archive.bulkUpdated", { count })); }} aria-label={t("archive.bulkAddTag")} options={taxonomy.tags.map((tag) => ({ value: tag.id, label: tag.name }))} />
            ) : null}
            <button type="button" className="button button--danger" disabled={Boolean(bulk)} onClick={() => void runBulk((id) => api.trashDocument(id), (count) => t("archive.bulkTrashed", { count }), ([id]) => api.restoreDocument(id))}><Trash2 aria-hidden="true" />{t("archive.bulkTrash")}</button>
          </div> : null}
          <button type="button" className="icon-button" disabled={Boolean(bulk)} onClick={stopSelecting} aria-label={t("archive.selectDone")}><X /></button>
        </div>
      ) : null}

      {loadMoreError ? (
        <div className="inline-error load-more-error" role="alert">
          <p>{t("archive.loadMoreFailed")}</p>
          <button type="button" className="button button--secondary" onClick={() => void loadMore()} disabled={loadingMore}>
            {t("common.retry")}
          </button>
        </div>
      ) : null}

      {!loadMoreError && nextCursor && !loading ? (
        <button type="button" className="load-more" onClick={() => void loadMore()} disabled={loadingMore}>
          <ArrowDown />{loadingMore ? t("archive.loading") : t("archive.loadMore")}
        </button>
      ) : null}
    </div>
  );
}
