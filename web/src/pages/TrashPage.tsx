import { useQuery, useQueryClient } from "@tanstack/react-query";
import { FileText, RotateCcw, Trash2 } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { ApiError, api, type DocumentView } from "../api";
import { useConfirm } from "../components/controls/ConfirmDialog";
import { toast } from "../components/controls/toast";
import { invalidateArchive, trashQuery } from "../queries";
import { TopBar } from "../shell/PageHead";

export function TrashPage() {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();
  const { data: documents, error: queryError, refetch } = useQuery(trashQuery);
  const [actionError, setActionError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const language = i18n.resolvedLanguage ?? "en";

  const error = actionError ?? (queryError instanceof ApiError ? queryError.code : queryError ? "network" : null);

  async function run(key: string, action: () => Promise<void>) {
    setBusy(key);
    setActionError(null);
    try {
      await action();
      await invalidateArchive(queryClient);
    } catch (cause) {
      setActionError(cause instanceof ApiError ? cause.code : "network");
    } finally {
      setBusy(null);
    }
  }

  const { confirm, dialogElement } = useConfirm();

  function restore(document: DocumentView) {
    void run(document.id, async () => {
      await api.restoreDocument(document.id);
      toast(t("trash.restoredToast"), { icon: "check" });
    });
  }

  async function purge(document: DocumentView) {
    const title = document.title ?? document.file.original_name;
    const ok = await confirm({
      title: t("trash.purgeFor", { title }),
      description: t("trash.purgeConfirm"),
      confirmLabel: t("trash.purge"),
      destructive: true,
    });
    if (!ok) return;

    void run(document.id, async () => {
      await api.purgeDocument(document.id);
      toast(t("trash.purgedToast"), { icon: "check" });
    });
  }

  async function empty() {
    if (!documents?.length) return;
    const ok = await confirm({
      title: t("trash.empty"),
      description: t("trash.emptyConfirm", { count: documents.length }),
      confirmLabel: t("trash.empty"),
      destructive: true,
    });
    if (!ok) return;

    void run("empty", async () => {
      await api.emptyTrash();
      toast(t("trash.emptiedToast"), { icon: "check" });
    });
  }

  return (
    <div className="trash-page">
      <TopBar title={t("trash.title")} />
      <header className="management-header">
        <div><h1>{t("trash.title")}</h1><p>{t("trash.intro")}</p></div>
        {documents && documents.length > 0 ? (
          <button type="button" className="button button--danger" disabled={busy !== null} onClick={empty}><Trash2 />{t("trash.empty")}</button>
        ) : null}
      </header>
      {documents && documents.length > 0 ? (
        <div className="trash-notice"><Trash2 /><p>{t("trash.notice")}</p></div>
      ) : null}
      {error ? <div className="inline-error" role="alert"><p>{t(`errors.${error}`, { defaultValue: t("errors.trash") })}</p><button type="button" className="button button--secondary" onClick={() => { setActionError(null); void refetch(); }}>{t("common.retry")}</button></div> : null}
      {!documents && !error ? <div className="management-loading" role="status">{t("trash.loading")}</div> : null}
      {documents?.length === 0 ? <section className="management-empty"><FileText /><h2>{t("trash.emptyTitle")}</h2><p>{t("trash.emptyDescription")}</p></section> : null}
      {documents && documents.length > 0 ? (
        <ul className="trash-list">
          {documents.map((document) => {
            const title = document.title ?? document.file.original_name;
            return (
              <li key={document.id}>
                <div className="trash-document-mark">
                  <FileText />
                  {document.file.has_pages ? (
                    <img
                      src={api.thumbnailUrl(document.id)}
                      alt=""
                      loading="lazy"
                      decoding="async"
                      ref={(node) => { if (node?.complete && node.naturalWidth > 0) node.classList.add("is-loaded"); }}
                      onLoad={(event) => event.currentTarget.classList.add("is-loaded")}
                      onError={(event) => { event.currentTarget.hidden = true; }}
                    />
                  ) : null}
                </div>
                <div className="trash-document-body">
                  <h2>{title}</h2>
                  <p>{document.file.original_name}</p>
                  <time dateTime={document.trashed_at ?? undefined}>{t("trash.removed", { date: new Intl.DateTimeFormat(language, { dateStyle: "medium" }).format(new Date(document.trashed_at ?? document.updated_at)) })}</time>
                </div>
                <div className="trash-actions">
                  <button type="button" className="button button--secondary" disabled={busy !== null} onClick={() => restore(document)} aria-label={t("trash.restoreFor", { title })}><RotateCcw />{t("trash.restore")}</button>
                  <button type="button" className="quiet-button trash-purge" disabled={busy !== null} onClick={() => purge(document)} aria-label={t("trash.purgeFor", { title })}><Trash2 />{t("trash.purge")}</button>
                </div>
              </li>
            );
          })}
        </ul>
      ) : null}
      {dialogElement}
    </div>
  );
}
