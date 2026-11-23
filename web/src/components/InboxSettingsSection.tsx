import { useQuery, useQueryClient } from "@tanstack/react-query";
import { FolderInput, PauseCircle } from "lucide-react";
import { useId, useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";

import { ApiError, api, type AdminInboxSettings, type AdminSettingsPatch } from "../api";
import { formatFileSize } from "../format";
import { adminSettingsQuery, usersQuery } from "../queries";
import { SelectDropdown } from "./controls/SelectDropdown";
import { toast } from "./controls/toast";

function errorCode(cause: unknown): string {
  return cause instanceof ApiError ? cause.code : "network";
}

function formatFailedAt(value: string | null, language: string): string | null {
  if (!value) return null;
  const parsed = new Date(value);
  if (Number.isNaN(parsed.getTime())) return value;
  return new Intl.DateTimeFormat(language, { dateStyle: "medium", timeStyle: "short" }).format(parsed);
}

function isDraftDirty(
  baseline: AdminInboxSettings | null | undefined,
  draft: { paused: boolean; userId: string; intervalSecs: string },
): boolean {
  if (!baseline || !baseline.configured) return false;
  return (
    draft.paused !== baseline.paused
    || (draft.userId || null) !== baseline.user_id
    || draft.intervalSecs.trim() !== String(baseline.interval_secs)
  );
}

/**
 * Administrator controls for the watched inbox folder and a viewer for files
 * moved to `.failed/` when ingestion fails.
 */
export function InboxSettingsSection({
  onError,
}: {
  onError: (code: string | null) => void;
}) {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();
  const { data } = useQuery(adminSettingsQuery);
  const { data: users } = useQuery(usersQuery);
  const language = i18n.resolvedLanguage ?? "en";

  const intervalId = useId();
  const [busy, setBusy] = useState(false);
  const [prevData, setPrevData] = useState<AdminInboxSettings | null | undefined>(undefined);
  const [paused, setPaused] = useState(false);
  const [userId, setUserId] = useState("");
  const [intervalSecs, setIntervalSecs] = useState("15");

  if (data && data.inbox !== prevData) {
    const wasDirty = isDraftDirty(prevData, { paused, userId, intervalSecs });
    setPrevData(data.inbox);
    if (!wasDirty) {
      setPaused(data.inbox.paused);
      setUserId(data.inbox.user_id ?? "");
      setIntervalSecs(String(data.inbox.interval_secs));
    }
  }

  if (data === null) return null;

  const inbox = data?.inbox;
  const dirty = isDraftDirty(inbox, { paused, userId, intervalSecs });
  const activeUsers = (users ?? []).filter((user) => user.disabled_at === null);
  const userOptions = [
    { value: "", label: t("settings.inboxUserAuto") },
    ...activeUsers.map((user) => ({ value: user.id, label: user.display_name })),
  ];

  const statusLabel = inbox?.configured
    ? inbox.paused
      ? t("settings.inboxStatusPaused")
      : t("settings.inboxStatusWatching")
    : null;
  const statusClass = inbox?.configured && !inbox.paused ? "settings-badge" : "settings-badge settings-badge--plain";

  async function saveSettings(event: FormEvent) {
    event.preventDefault();
    if (!inbox) return;
    setBusy(true);
    onError(null);
    const inboxPatch: NonNullable<AdminSettingsPatch["inbox"]> = {};
    if (paused !== inbox.paused) inboxPatch.paused = paused;
    if ((userId || null) !== inbox.user_id) inboxPatch.user_id = userId || null;
    if (intervalSecs.trim() !== String(inbox.interval_secs)) {
      const trimmed = intervalSecs.trim();
      const parsedInterval = trimmed === "" ? -1 : Number(trimmed);
      if (!Number.isInteger(parsedInterval) || parsedInterval < 0) {
        onError("invalid_inbox_interval");
        setBusy(false);
        return;
      }
      inboxPatch.interval_secs = parsedInterval;
    }
    try {
      const updated = await api.updateSettings({
        inbox: inboxPatch,
      });
      queryClient.setQueryData(adminSettingsQuery.queryKey, updated);
      setPrevData(updated.inbox);
      setPaused(updated.inbox.paused);
      setUserId(updated.inbox.user_id ?? "");
      setIntervalSecs(String(updated.inbox.interval_secs));
      await queryClient.invalidateQueries({ queryKey: ["settings-summary"] });
      toast(t("settings.inboxSavedToast"), { icon: "check" });
    } catch (cause) {
      onError(errorCode(cause));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="settings-section" aria-labelledby="settings-inbox">
      <div className="settings-section-heading">
        <div>
          <h3 id="settings-inbox">{t("settings.inboxTitle")}</h3>
          <p>{t("settings.inboxDescription")}</p>
        </div>
        {statusLabel ? <span className={statusClass}>{statusLabel}</span> : null}
      </div>

      {inbox && !inbox.configured ? (
        <div className="settings-info-card">
          <FolderInput aria-hidden="true" />
          <div>
            <strong>{t("settings.inboxUnconfiguredTitle")}</strong>
            <p>{t("settings.inboxUnconfiguredDescription")}</p>
          </div>
        </div>
      ) : null}

      {inbox && inbox.configured ? (
        <div className="settings-stack">
          <form className="settings-form" onSubmit={saveSettings} noValidate>
            <label className="toggle-row">
              <span className="toggle-row-text">
                <PauseCircle aria-hidden="true" />
                <span>
                  <strong>{t("settings.inboxPaused")}</strong>
                  <small>{t("settings.inboxPausedHint")}</small>
                </span>
              </span>
              <input
                type="checkbox"
                role="switch"
                className="switch"
                checked={paused}
                onChange={(event) => setPaused(event.target.checked)}
                aria-label={t("settings.inboxPaused")}
              />
            </label>

            <div className="settings-form-grid">
              <div className="field-label">
                <span>{t("settings.inboxTargetUser")}</span>
                <SelectDropdown
                  value={userId}
                  onChange={setUserId}
                  placeholder={t("settings.inboxUserAuto")}
                  aria-label={t("settings.inboxTargetUser")}
                  options={userOptions}
                />
              </div>
              <label className="field-label" htmlFor={intervalId}>
                <span>{t("settings.inboxInterval")}</span>
                <input
                  id={intervalId}
                  type="number"
                  min={1}
                  max={86400}
                  value={intervalSecs}
                  onChange={(event) => setIntervalSecs(event.target.value)}
                  aria-label={t("settings.inboxInterval")}
                />
              </label>
            </div>

            <div className="settings-form-footer">
              <div>
                {dirty ? <span className="taxonomy-dirty" role="status">{t("taxonomy.unsaved")}</span> : null}
              </div>
              <div className="settings-form-actions">
                <button type="submit" className="button button--primary" disabled={busy || !dirty}>
                  {t("settings.inboxSave")}
                </button>
              </div>
            </div>
          </form>

          <div className="inbox-failed-block">
            <h4>{t("settings.inboxFailedTitle")}</h4>
            {inbox.failed_files.length === 0 ? (
              <p className="settings-note">{t("settings.inboxNoFailed")}</p>
            ) : (
              <ul className="settings-records">
                {inbox.failed_files.map((file, index) => {
                  const when = formatFailedAt(file.failed_at, language);
                  const meta = [formatFileSize(file.size, language), when].filter(Boolean).join(" · ");
                  return (
                    <li key={`${file.name}:${index}`}>
                      <div>
                        <strong>{file.name}</strong>
                        <span>{meta}</span>
                        {file.reason ? <span className="inbox-failed-reason">{file.reason}</span> : null}
                      </div>
                    </li>
                  );
                })}
              </ul>
            )}
          </div>
        </div>
      ) : null}
    </section>
  );
}
