import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Copy, Link2, LoaderCircle, Send, X } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { ApiError, api, type Share, type ShareTtl } from "../api";
import { Sheet } from "../shell/Sheet";
import { SegmentedControl } from "./controls/SegmentedControl";
import { toast } from "./controls/toast";

const TTLS: ShareTtl[] = ["15m", "1h", "24h"];

export const sharesQuery = (id: string) => ({
  queryKey: ["shares", id] as const,
  queryFn: () => api.shares(id),
  // A link's lifetime is short; what is live is asked again each time.
  staleTime: 0,
  gcTime: 0,
});

/** The share sheet of a document: makes a link to a page that shows the document to anyone who
 *  has it (with or without its extracted values) for 15 minutes, an hour or a day, and lists the
 *  links that still work, to copy, send or revoke. */
export function ShareSheet({ documentId, title, open, onClose }: {
  documentId: string;
  title: string;
  open: boolean;
  onClose: () => void;
}) {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();
  const [ttl, setTtl] = useState<ShareTtl>("15m");
  const [showDetails, setShowDetails] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const language = i18n.resolvedLanguage ?? "en";
  const { data: shares, isPending } = useQuery({ ...sharesQuery(documentId), enabled: open });
  const canSend = typeof navigator !== "undefined" && typeof navigator.share === "function";

  const failed = (cause: unknown) => setError(cause instanceof ApiError ? cause.code : "network");

  const create = useMutation({
    mutationFn: () => api.createShare(documentId, { ttl, show_details: showDetails }),
    onMutate: () => setError(null),
    onSuccess: async (share) => {
      queryClient.setQueryData<Share[]>(sharesQuery(documentId).queryKey, (current) => [share, ...(current ?? [])]);
      // Copied straight away: the link is what was asked for.
      await copy(share, false);
    },
    onError: failed,
  });

  const revoke = useMutation({
    mutationFn: (share: Share) => api.revokeShare(share.id),
    onMutate: () => setError(null),
    onSuccess: (_result, share) => {
      queryClient.setQueryData<Share[]>(sharesQuery(documentId).queryKey, (current) => (current ?? []).filter((item) => item.id !== share.id));
      toast(t("document.shareRevokedToast"), { icon: "check" });
    },
    onError: failed,
  });

  async function copy(share: Share, announceFailure = true) {
    try {
      await navigator.clipboard.writeText(share.url);
      toast(t("document.copiedToast"), { icon: "check" });
    } catch {
      if (announceFailure) setError("clipboard");
    }
  }

  async function send(share: Share) {
    try {
      await navigator.share({ title, url: share.url });
    } catch {
      // Closing the system share sheet is not an error.
    }
  }

  // A day-long link may end tomorrow: its end then names the day too.
  const until = (share: Share) => new Intl.DateTimeFormat(language, { hour: "2-digit", minute: "2-digit", ...(Date.parse(share.expires_at) - Date.parse(share.created_at) > 20 * 3600 * 1000 ? { day: "numeric", month: "short" } : {}) }).format(new Date(share.expires_at));
  // The server lists only links that still work, and the list is asked again on every opening.
  const live = shares ?? [];

  return (
    <Sheet
      open={open}
      title={t("document.shareSheetTitle")}
      onClose={onClose}
      headerAction={<button type="button" className="icon-button" onClick={onClose} aria-label={t("common.close")} title={t("common.close")}><X aria-hidden="true" /></button>}
    >
      <div className="share-sheet">
        <p className="share-sheet-lead">{t("document.shareLead")}</p>

        <div className="share-sheet-field">
          <span id="share-ttl-label">{t("document.shareTtlLabel")}</span>
          <SegmentedControl
            value={ttl}
            onChange={setTtl}
            ariaLabel={t("document.shareTtlLabel")}
            options={TTLS.map((value) => ({ value, label: t(`document.shareTtl.${value}`) }))}
          />
        </div>

        <label className="share-sheet-toggle">
          <input type="checkbox" role="switch" className="switch" checked={showDetails} onChange={(event) => setShowDetails(event.target.checked)} />
          <span>
            <strong>{t("document.shareDetails")}</strong>
            <small>{t(showDetails ? "document.shareDetailsOn" : "document.shareDetailsOff")}</small>
          </span>
        </label>

        <button type="button" className="button button--primary share-sheet-create" disabled={create.isPending} onClick={() => create.mutate()}>
          {create.isPending ? <LoaderCircle className="spin" aria-hidden="true" /> : <Link2 aria-hidden="true" />}
          {t("document.shareCreate")}
        </button>

        {error ? <p className="share-sheet-error" role="alert">{t(`errors.${error}`, { defaultValue: t("errors.document") })}</p> : null}

        <section className="share-sheet-live" aria-label={t("document.shareLive")}>
          <h3>{t("document.shareLive")}</h3>
          {isPending && open ? <p className="share-sheet-empty"><LoaderCircle className="spin" aria-hidden="true" /></p> : null}
          {!isPending && live.length === 0 ? <p className="share-sheet-empty">{t("document.shareNone")}</p> : null}
          {live.length > 0 ? (
            <ul>
              {live.map((share) => (
                <li key={share.id}>
                  <div>
                    <strong>{t("document.shareUntil", { time: until(share) })}</strong>
                    <small>{t(share.show_details ? "document.shareWithDetails" : "document.shareWithoutDetails")}</small>
                  </div>
                  {canSend ? <button type="button" className="icon-button" onClick={() => void send(share)} aria-label={t("document.shareSend")} title={t("document.shareSend")}><Send /></button> : null}
                  <button type="button" className="icon-button" onClick={() => void copy(share)} aria-label={t("document.copy")} title={t("document.copy")}><Copy /></button>
                  <button type="button" className="icon-button" disabled={revoke.isPending} onClick={() => revoke.mutate(share)} aria-label={t("document.shareRevoke")} title={t("document.shareRevoke")}><X /></button>
                </li>
              ))}
            </ul>
          ) : null}
        </section>
      </div>
    </Sheet>
  );
}
