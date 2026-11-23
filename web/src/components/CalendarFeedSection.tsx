import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, Copy, Plus, RefreshCw, Trash2 } from "lucide-react";
import { useId, useRef, useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";

import { ApiError, api } from "../api";
import { adminSettingsQuery, calendarFeedQuery } from "../queries";
import type { useConfirm } from "./controls/ConfirmDialog";
import { toast } from "./controls/toast";

function formatDate(value: string | null, language: string, never: string): string {
  return value ? new Intl.DateTimeFormat(language, { dateStyle: "medium" }).format(new Date(value)) : never;
}

/**
 * The calendar subscription: the due dates of the archive as a link a calendar app polls.
 * The link holds a secret that the server keeps only as a hash, so it is shown once, when it is made.
 */
export function CalendarFeedSection({
  admin = false,
  confirm,
  onError,
}: {
  admin?: boolean;
  confirm: ReturnType<typeof useConfirm>["confirm"];
  onError: (code: string | null) => void;
}) {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();
  const { data } = useQuery(calendarFeedQuery);
  const language = i18n.resolvedLanguage ?? "en";
  const linkInput = useRef<HTMLInputElement>(null);
  const leadInputId = useId();
  const [link, setLink] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [busy, setBusy] = useState(false);
  const [prevLeadDays, setPrevLeadDays] = useState<number | undefined>(undefined);
  const [leadDaysInput, setLeadDaysInput] = useState("60");

  if (data && data.lead_days !== prevLeadDays) {
    const wasDirty = prevLeadDays !== undefined && leadDaysInput.trim() !== String(prevLeadDays);
    setPrevLeadDays(data.lead_days);
    if (!wasDirty) {
      setLeadDaysInput(String(data.lead_days));
    }
  }

  const leadDirty = data !== undefined && leadDaysInput.trim() !== String(data.lead_days);

  async function saveLeadDays(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    onError(null);
    const trimmed = leadDaysInput.trim();
    const parsed = Number(trimmed);
    if (trimmed === "" || !Number.isInteger(parsed) || parsed < 0) {
      onError("invalid_calendar_lead_days");
      setBusy(false);
      return;
    }
    try {
      const updated = await api.updateSettings({ calendar: { lead_days: parsed } });
      queryClient.setQueryData(adminSettingsQuery.queryKey, updated);
      setPrevLeadDays(updated.calendar.lead_days);
      setLeadDaysInput(String(updated.calendar.lead_days));
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["calendar-feed"] }),
        queryClient.invalidateQueries({ queryKey: ["settings"] }),
        queryClient.invalidateQueries({ queryKey: ["settings-summary"] }),
      ]);
      toast(t("settings.calendarLeadSavedToast"), { icon: "check" });
    } catch (cause) {
      onError(cause instanceof ApiError ? cause.code : "network");
    } finally {
      setBusy(false);
    }
  }

  async function make(rotated: boolean) {
    if (rotated) {
      const ok = await confirm({
        title: t("settings.calendarRotateTitle"),
        description: t("settings.calendarRotateConfirm"),
        confirmLabel: t("settings.calendarRotate"),
        cancelLabel: t("common.cancel"),
        destructive: true,
      });
      if (!ok) return;
    }
    setBusy(true);
    onError(null);
    setCopied(false);
    try {
      const created = await api.createCalendarFeed();
      setLink(created.url ?? `${window.location.origin}${created.path}`);
      await queryClient.invalidateQueries({ queryKey: ["calendar-feed"] });
      toast(t(rotated ? "settings.calendarRotatedToast" : "settings.calendarCreatedToast"), { icon: "check" });
    } catch (cause) {
      onError(cause instanceof ApiError ? cause.code : "network");
    } finally {
      setBusy(false);
    }
  }

  async function revoke() {
    const ok = await confirm({
      title: t("settings.calendarRevokeTitle"),
      description: t("settings.calendarRevokeConfirm"),
      confirmLabel: t("settings.revoke"),
      cancelLabel: t("common.cancel"),
      destructive: true,
    });
    if (!ok) return;
    setBusy(true);
    onError(null);
    try {
      await api.revokeCalendarFeed();
      setLink(null);
      await queryClient.invalidateQueries({ queryKey: ["calendar-feed"] });
      toast(t("settings.calendarRevokedToast"), { icon: "check" });
    } catch (cause) {
      onError(cause instanceof ApiError ? cause.code : "network");
    } finally {
      setBusy(false);
    }
  }

  async function copyLink() {
    if (!link) return;
    try {
      await navigator.clipboard.writeText(link);
      setCopied(true);
      toast(t("settings.calendarCopiedToast"), { icon: "check" });
    } catch {
      linkInput.current?.select();
      onError("clipboard");
    }
  }

  const feed = data?.feed ?? null;
  return (
    <section className="settings-section" aria-labelledby="settings-calendar">
      <div className="settings-section-heading">
        <div><h3 id="settings-calendar">{t("settings.calendar")}</h3><p>{t("settings.calendarDescription")}</p></div>
        {data && !feed ? <button type="button" className="button button--secondary" disabled={busy} onClick={() => void make(false)}><Plus aria-hidden="true" />{t("settings.calendarCreate")}</button> : null}
      </div>
      {link ? (
        <div className="token-secret" role="status">
          <div><strong>{t("settings.calendarLinkTitle")}</strong><p>{t("settings.calendarLinkDescription")}</p></div>
          <div className="token-secret-row">
            <input ref={linkInput} readOnly value={link} aria-label={t("settings.calendarLinkLabel")} onFocus={(event) => event.currentTarget.select()} />
            <button type="button" className="button button--secondary" onClick={() => void copyLink()}>{copied ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}{copied ? t("document.copied") : t("settings.calendarCopy")}</button>
          </div>
        </div>
      ) : null}
      {feed ? (
        <ul className="settings-records token-records">
          <li>
            <div>
              <strong>{t("settings.calendarActive")}</strong>
              <span>{t("settings.created", { date: formatDate(feed.created_at, language, t("settings.never")) })}</span>
              <span>{t("settings.calendarLastFetched", { date: formatDate(feed.last_used_at, language, t("settings.never")) })}</span>
            </div>
            <div>
              <button type="button" className="quiet-button" disabled={busy} onClick={() => void make(true)}><RefreshCw aria-hidden="true" />{t("settings.calendarRotate")}</button>
              <button type="button" className="quiet-button" disabled={busy} onClick={() => void revoke()} aria-label={t("settings.calendarRevokeTitle")}><Trash2 aria-hidden="true" />{t("settings.revoke")}</button>
            </div>
          </li>
        </ul>
      ) : null}
      {admin && data ? (
        <form className="calendar-lead-form" onSubmit={saveLeadDays} noValidate>
          <div className="calendar-lead-row">
            <label className="field-label" htmlFor={leadInputId}>
              <span>{t("settings.calendarLeadLabel")}</span>
              <input
                id={leadInputId}
                type="number"
                min={0}
                max={3650}
                value={leadDaysInput}
                onChange={(event) => setLeadDaysInput(event.target.value)}
                aria-label={t("settings.calendarLeadLabel")}
              />
            </label>
            <button type="submit" className="button button--secondary" disabled={busy || !leadDirty}>
              {t("settings.calendarLeadSave")}
            </button>
          </div>
          <p className="email-field-hint">{t("settings.calendarLeadAdminHint")}</p>
        </form>
      ) : data && data.lead_days > 0 ? (
        <p className="settings-note">{t("settings.calendarLead", { count: data.lead_days })}</p>
      ) : null}
      <p className="settings-note">{t("settings.calendarHelp")}</p>
    </section>
  );
}
