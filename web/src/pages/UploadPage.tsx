import { AlertCircle, ArrowRight, Check, ChevronDown, CloudOff, File, FileImage, LoaderCircle, RotateCcw, Upload, X } from "lucide-react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState, type ChangeEvent, type DragEvent } from "react";
import { useTranslation } from "react-i18next";

import { SelectDropdown } from "../components/controls/SelectDropdown";
import { toggleId } from "../document";
import { formatFileSize } from "../format";
import { invalidateArchive, taxonomyQuery } from "../queries";
import { documentPath } from "../shell/nav";
import { ScreenLink } from "../shell/navigation";
import { TopBar } from "../shell/PageHead";
import { runUpload, type UploadUpdate } from "../upload";

type UploadPhase = "waiting" | UploadUpdate["phase"];

interface UploadEntry {
  key: string;
  file: File;
  // null follows the batch setting; a boolean is an explicit per-file choice.
  localOnly: boolean | null;
  // What the upload actually used, kept for the status badge once it has started.
  sentLocalOnly?: boolean;
  phase: UploadPhase;
  documentId?: string;
  title?: string | null;
  restored?: boolean;
  code?: string;
  progress?: number;
}

interface Prefill {
  typeId: string;
  people: string[];
  tags: string[];
}

const emptyPrefill: Prefill = { typeId: "", people: [], tags: [] };
const finishedPhases: UploadPhase[] = ["ready", "duplicate", "failed"];
const activePhases: UploadPhase[] = ["uploading", "queued", "processing"];
/** Failures the same file would hit again, so offering "Try again" would only mislead. */
const PERMANENT_FAILURES = new Set(["unsupported_type", "empty_file", "file_too_large"]);

export function UploadPage() {
  const { t, i18n } = useTranslation();
  const [entries, setEntries] = useState<UploadEntry[]>([]);
  const [dragging, setDragging] = useState(false);
  const [prefill, setPrefill] = useState<Prefill>(emptyPrefill);
  const [batchLocalOnly, setBatchLocalOnly] = useState(false);
  const { data: taxonomy } = useQuery(taxonomyQuery);
  const queryClient = useQueryClient();
  const mounted = useRef(true);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  function addFiles(files: File[]) {
    if (files.length === 0) return;
    setEntries((current) => [
      ...current,
      ...files.map((file) => ({ key: crypto.randomUUID(), file, localOnly: null, phase: "waiting" as const })),
    ]);
  }

  function chooseFiles(event: ChangeEvent<HTMLInputElement>) {
    addFiles(Array.from(event.target.files ?? []));
    event.target.value = "";
  }

  function dropFiles(event: DragEvent<HTMLLabelElement>) {
    event.preventDefault();
    setDragging(false);
    addFiles(Array.from(event.dataTransfer.files));
  }

  function updateEntry(key: string, update: UploadUpdate) {
    if (!mounted.current) return;
    // Progress belongs to one upload attempt, so a new attempt starts without a stale value.
    setEntries((current) => current.map((entry) => entry.key === key ? { ...entry, progress: undefined, ...update } : entry));
  }

  function start(entriesToUpload: UploadEntry[]) {
    const waiting = entriesToUpload.filter((entry) => entry.phase === "waiting");
    // A lone file shows no per-file choice, so the switch alone decides for it.
    const effective = new Map(waiting.map((entry) => [entry.key, waiting.length > 1 ? entry.localOnly ?? batchLocalOnly : batchLocalOnly]));
    setEntries((current) => current.map((entry) => effective.has(entry.key) ? { ...entry, sentLocalOnly: effective.get(entry.key) } : entry));
    for (const entry of waiting) {
      let stored = false;
      void runUpload(
        entry.file,
        { localOnly: effective.get(entry.key) ?? false, typeId: prefill.typeId || undefined, people: [...prefill.people], tags: [...prefill.tags] },
        (update) => {
          // A document that is stored, and again when it has settled (a processed one brings its
          // type, people and organizations), changes the archive's lists and counts. This runs even
          // if the page was left meanwhile: the upload carries on.
          const settled = update.phase === "ready" || update.phase === "failed";
          if (update.documentId && (settled || !stored)) void invalidateArchive(queryClient);
          if (update.documentId) stored = true;
          updateEntry(entry.key, update);
        },
      );
    }
  }

  const waitingCount = entries.filter((entry) => entry.phase === "waiting").length;
  const activeCount = entries.filter((entry) => activePhases.includes(entry.phase)).length;
  const finishedCount = entries.filter((entry) => finishedPhases.includes(entry.phase)).length;
  const addedCount = entries.filter((entry) => entry.phase === "ready").length;
  const duplicateCount = entries.filter((entry) => entry.phase === "duplicate").length;
  const failedCount = entries.filter((entry) => entry.phase === "failed").length;
  const hasImage = entries.some((entry) => entry.file.type.startsWith("image/"));
  // One stage for the whole batch: unsent files come first because they are the next thing the user can act on.
  const stage = waitingCount > 0 ? "Ready" : activeCount > 0 ? "Active" : addedCount > 0 ? "Added" : failedCount > 0 ? "NotAdded" : "Duplicate";
  // The header names the stage with its count; the summary line lists only the other counts, so nothing is said twice.
  const stageCount = { Ready: waitingCount, Active: activeCount, Added: addedCount, NotAdded: failedCount, Duplicate: duplicateCount }[stage];
  const summary = [
    stage !== "Active" && activeCount > 0 ? t("upload.countActive", { count: activeCount }) : null,
    stage !== "Added" && addedCount > 0 ? t("upload.countAdded", { count: addedCount }) : null,
    stage !== "Duplicate" && duplicateCount > 0 ? t("upload.countDuplicate", { count: duplicateCount }) : null,
    stage !== "NotAdded" && failedCount > 0 ? t("upload.countFailed", { count: failedCount }) : null,
  ].filter(Boolean).join(" · ");
  const batchMode = t(batchLocalOnly ? "upload.privacyModeLocal" : "upload.privacyModeCloud");
  const language = i18n.resolvedLanguage ?? "en";
  const types = taxonomy?.types ?? [];
  const people = taxonomy?.people ?? [];
  const tags = taxonomy?.tags ?? [];
  const typeLabel = (type: { key: string; labels: Record<string, string> }) => type.labels[language] ?? type.labels.en ?? type.key;
  const chosenType = types.find((type) => type.id === prefill.typeId);
  const chosenNames = [
    ...(chosenType ? [typeLabel(chosenType)] : []),
    ...people.filter((person) => prefill.people.includes(person.id)).map((person) => person.name),
    ...tags.filter((tag) => prefill.tags.includes(tag.id)).map((tag) => tag.name),
  ];

  return (
    <div className="upload-page">
      <TopBar title={t("upload.title")} />
      <header className="upload-header">
        <h1>{t("upload.title")}</h1>
        <p>{t("upload.intro")}</p>
      </header>

      <label
        className={dragging ? "upload-dropzone upload-dropzone--active" : "upload-dropzone"}
        onDragOver={(event) => { event.preventDefault(); setDragging(true); }}
        onDragLeave={() => setDragging(false)}
        onDrop={dropFiles}
      >
        <input
          ref={inputRef}
          className="upload-file-input"
          type="file"
          multiple
          accept="application/pdf,image/*,.txt,.md,text/plain,text/markdown"
          onChange={chooseFiles}
          aria-label={t("upload.pick")}
        />
        <Upload aria-hidden="true" />
        <strong className="upload-drop-pointer">{t("upload.dropTitle")}</strong>
        <strong className="upload-drop-touch">{t("upload.dropTitleTouch")}</strong>
        <span className="upload-drop-pointer">{t("upload.dropDescription")}</span>
        <span className="button button--primary upload-pick">{t("upload.pick")}</span>
        <small><span>{t("upload.formatsLabel")}</span> {t("upload.formats")}</small>
      </label>

      <aside className={hasImage ? "scan-hint scan-hint--shown" : "scan-hint"}>
        <FileImage />
        <div><strong>{t("upload.scanTitle")}</strong><p>{t("upload.scanHint")}</p></div>
      </aside>

      {entries.length > 0 ? (
        <section className="upload-queue" aria-labelledby="upload-queue-title">
          <div className="upload-queue-heading">
            <div>
              <h2 id="upload-queue-title" className={`upload-stage upload-stage--${stage.toLowerCase()}`}>{t("upload.stageWithCount", { stage: t(`upload.stage${stage}`), number: stageCount })}</h2>
              {summary ? <p className="upload-summary" aria-live="polite">{summary}</p> : null}
            </div>
            {finishedCount > 0 ? <button type="button" className="quiet-button" onClick={() => setEntries((current) => current.filter((entry) => !finishedPhases.includes(entry.phase)))}>{t("upload.clearFinished")}</button> : null}
          </div>
          {waitingCount > 0 ? (
            <>
              <label className="toggle-row upload-batch-privacy">
                <span className="toggle-row-text">
                  <CloudOff aria-hidden="true" />
                  <span><strong>{t("upload.localOnly")}</strong><small>{t("upload.localOnlyDescription")}{waitingCount > 1 ? ` ${t("upload.localOnlyBatchNote")}` : null}</small></span>
                </span>
                <input type="checkbox" role="switch" className="switch" checked={batchLocalOnly} onChange={(event) => setBatchLocalOnly(event.target.checked)} aria-label={t("upload.localOnly")} />
              </label>
              <details className="upload-prefill">
                <summary>
                  <ChevronDown className="disclosure-chevron" aria-hidden="true" />
                  {t("upload.prefillTitle")}
                  <span className="upload-prefill-optional">{t("upload.prefillOptional")}</span>
                  {chosenNames.length > 0 ? <span className="upload-prefill-summary"> — {chosenNames.join(", ")}</span> : null}
                </summary>
                <p className="upload-prefill-hint">{t("upload.prefillHint")}</p>
                <div className="field-label upload-prefill-type">
                  <span>{t("upload.prefillType")}</span>
                  <SelectDropdown
                    value={prefill.typeId}
                    onChange={(typeId) => setPrefill((current) => ({ ...current, typeId }))}
                    options={[{ value: "", label: t("upload.prefillNone") }, ...types.map((type) => ({ value: type.id, label: typeLabel(type) }))]}
                    aria-label={t("upload.prefillType")}
                  />
                </div>
                <fieldset className="upload-prefill-group">
                  <legend>{t("upload.prefillPeople")}</legend>
                  {people.length === 0 ? <p className="upload-prefill-empty">{t("upload.prefillNoPeople")}</p> : null}
                  {people.map((person) => (
                    <label className="upload-prefill-option" key={person.id}>
                      <input type="checkbox" checked={prefill.people.includes(person.id)} onChange={(event) => setPrefill((current) => ({ ...current, people: toggleId(current.people, person.id, event.target.checked) }))} />
                      <span>{person.name}</span>
                    </label>
                  ))}
                </fieldset>
                <fieldset className="upload-prefill-group">
                  <legend>{t("upload.prefillTags")}</legend>
                  {tags.length === 0 ? <p className="upload-prefill-empty">{t("upload.prefillNoTags")}</p> : null}
                  {tags.map((tag) => (
                    <label className="upload-prefill-option" key={tag.id}>
                      <input type="checkbox" checked={prefill.tags.includes(tag.id)} onChange={(event) => setPrefill((current) => ({ ...current, tags: toggleId(current.tags, tag.id, event.target.checked) }))} />
                      <span>{tag.name}</span>
                    </label>
                  ))}
                </fieldset>
              </details>
            </>
          ) : null}
          <ul className="upload-list">
            {entries.map((entry) => (
              <li className={`upload-item upload-item--${entry.phase}`} key={entry.key}>
                <div className="upload-file-icon">{entry.file.type.startsWith("image/") ? <FileImage /> : <File />}</div>
                <div className="upload-item-main">
                  {/* The extracted title is the row's name once known; the file name stays visible beneath it. */}
                  <div className="upload-file-heading">
                    <div>
                      {entry.documentId && finishedPhases.includes(entry.phase) ? (
                        // The icon link beside the row is the keyboard stop, with the full label; this one is for pointers.
                        <ScreenLink className="upload-title-link" to={documentPath(entry.documentId)} tabIndex={-1}><strong>{entry.title || entry.file.name}</strong></ScreenLink>
                      ) : <strong>{entry.title || entry.file.name}</strong>}
                      {entry.title ? <span className="upload-file-name">{entry.file.name}</span> : null}
                      <span>{formatFileSize(entry.file.size, language)}</span>
                    </div>
                    {entry.documentId && finishedPhases.includes(entry.phase) ? (
                      <ScreenLink className="icon-button upload-open" to={documentPath(entry.documentId)} aria-label={t("upload.openTitle", { title: entry.title || entry.file.name })}><ArrowRight aria-hidden="true" /></ScreenLink>
                    ) : null}
                    {entry.phase === "waiting" ? (
                      <button type="button" className="icon-button" onClick={() => setEntries((current) => current.filter((item) => item.key !== entry.key))} aria-label={t("upload.remove", { name: entry.file.name })}><X /></button>
                    ) : null}
                  </div>

                  {/* A file that has not started needs no status line: the queue's header already says so. */}
                  {entry.phase !== "waiting" ? (
                    <div className="upload-status" role="status">
                      {activePhases.includes(entry.phase) ? <LoaderCircle className="spin" /> : null}
                      {entry.phase === "ready" ? <Check /> : null}
                      {entry.phase === "failed" || entry.phase === "duplicate" ? <AlertCircle /> : null}
                      <span>
                        {entry.phase === "duplicate" ? t("upload.duplicate") : null}
                        {entry.phase === "failed" ? (entry.code ? t(`errors.${entry.code}`, { defaultValue: t("upload.failed") }) : t("upload.failed")) : null}
                        {!["duplicate", "failed"].includes(entry.phase) ? t(`upload.${entry.phase}`) : null}
                        {entry.phase === "uploading" && entry.progress !== undefined ? ` ${t("upload.percent", { percent: Math.round(entry.progress * 100) })}` : null}
                        {entry.restored ? ` · ${t("upload.restored")}` : null}
                      </span>
                      {entry.sentLocalOnly ? <span className="upload-local-badge"><CloudOff />{t("upload.localBadge")}</span> : null}
                    </div>
                  ) : null}

                  {entry.phase === "waiting" && waitingCount > 1 ? (
                    <SelectDropdown
                      className="upload-file-privacy"
                      aria-label={t("upload.privacyFile", { name: entry.file.name })}
                      value={entry.localOnly === null ? "inherit" : entry.localOnly ? "local" : "cloud"}
                      onChange={(choice) => {
                        setEntries((current) => current.map((item) => item.key === entry.key ? { ...item, localOnly: choice === "inherit" ? null : choice === "local" } : item));
                      }}
                      options={[
                        { value: "inherit", label: t("upload.privacyInherit", { mode: batchMode }) },
                        { value: "local", label: t("upload.privacyLocal") },
                        { value: "cloud", label: t("upload.privacyCloud") },
                      ]}
                    />
                  ) : null}

                  {entry.phase === "uploading" ? (
                    <div
                      className={entry.progress !== undefined ? "upload-progress upload-progress--determinate" : "upload-progress"}
                      role="progressbar"
                      aria-label={t("upload.uploading")}
                      aria-valuemin={0}
                      aria-valuemax={100}
                      aria-valuenow={entry.progress !== undefined ? Math.round(entry.progress * 100) : undefined}
                    >
                      <span style={entry.progress !== undefined ? { width: `${Math.round(entry.progress * 100)}%` } : undefined} />
                    </div>
                  ) : null}
                  {entry.phase === "failed" && !entry.documentId && !(entry.code && PERMANENT_FAILURES.has(entry.code)) ? <button type="button" className="quiet-button upload-retry" onClick={() => setEntries((current) => current.map((item) => item.key === entry.key ? { ...item, phase: "waiting", code: undefined } : item))}><RotateCcw />{t("common.retry")}</button> : null}
                </div>
              </li>
            ))}
          </ul>
          {/* Once the bytes are sent there is nothing to start; progress shows per file above. */}
          {waitingCount > 0 ? (
            <button type="button" className="button button--primary upload-start" onClick={() => start(entries)}>
              <Upload aria-hidden="true" />{t("upload.start", { count: waitingCount })}
            </button>
          ) : null}
        </section>
      ) : null}
    </div>
  );
}
