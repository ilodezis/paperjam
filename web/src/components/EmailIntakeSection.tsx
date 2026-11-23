import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, ChevronDown, CloudOff, Copy, FileText, Info, Mail, Plus, RefreshCw, Trash2 } from "lucide-react";
import { useId, useRef, useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";

import { ApiError, api, type EmailSettingsPatch, type EmailSettingsView } from "../api";
import { emailSettingsQuery, usersQuery } from "../queries";
import type { useConfirm } from "./controls/ConfirmDialog";
import { SelectDropdown } from "./controls/SelectDropdown";
import { toast } from "./controls/toast";

function formatDate(value: string | null, language: string, never: string): string {
  return value ? new Intl.DateTimeFormat(language, { dateStyle: "medium" }).format(new Date(value)) : never;
}

function errorCode(cause: unknown): string {
  return cause instanceof ApiError ? cause.code : "network";
}

function parseSenders(text: string): string[] {
  return text
    .split(/[\n,]+/)
    .map((entry) => entry.trim())
    .filter((entry) => entry.length > 0);
}

function arraysEqual(left: string[], right: string[]): boolean {
  return left.length === right.length && left.every((value, index) => value === right[index]);
}

function isDraftDirty(
  baseline: EmailSettingsView | null | undefined,
  draft: { allowedSendersText: string; userId: string; tag: string; bodyFallback: boolean; localOnly: boolean },
): boolean {
  if (!baseline) return false;
  return (
    !arraysEqual(parseSenders(draft.allowedSendersText), baseline.allowed_senders)
    || (draft.userId || null) !== baseline.user_id
    || (draft.tag.trim() || null) !== baseline.tag
    || draft.bodyFallback !== baseline.body_fallback
    || draft.localOnly !== baseline.local_only
  );
}

/**
 * Email intake settings for administrators: secret lifecycle, webhook endpoint,
 * Cloudflare Email Worker hints, and ingestion defaults.
 */
export function EmailIntakeSection({
  confirm,
  onError,
}: {
  confirm: ReturnType<typeof useConfirm>["confirm"];
  onError: (code: string | null) => void;
}) {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();
  const { data } = useQuery(emailSettingsQuery);
  const { data: users } = useQuery(usersQuery);
  const language = i18n.resolvedLanguage ?? "en";

  const secretInput = useRef<HTMLInputElement>(null);
  const endpointInput = useRef<HTMLInputElement>(null);
  const endpointId = useId();
  const sendersId = useId();
  const sendersHintId = useId();
  const tagId = useId();

  const [secret, setSecret] = useState<string | null>(null);
  const [copied, setCopied] = useState<"secret" | "endpoint" | null>(null);
  const [busy, setBusy] = useState(false);

  const [prevData, setPrevData] = useState<EmailSettingsView | null | undefined>(undefined);
  const [allowedSendersText, setAllowedSendersText] = useState("");
  const [userId, setUserId] = useState("");
  const [tag, setTag] = useState("");
  const [bodyFallback, setBodyFallback] = useState(false);
  const [localOnly, setLocalOnly] = useState(false);

  if (data && data !== prevData) {
    const wasDirty = isDraftDirty(prevData, { allowedSendersText, userId, tag, bodyFallback, localOnly });
    setPrevData(data);
    if (!wasDirty) {
      setAllowedSendersText(data.allowed_senders.join("\n"));
      setUserId(data.user_id ?? "");
      setTag(data.tag ?? "");
      setBodyFallback(data.body_fallback);
      setLocalOnly(data.local_only);
    }
  }

  if (data === null) return null;

  const parsedSenders = parseSenders(allowedSendersText);
  const dirty = isDraftDirty(data, { allowedSendersText, userId, tag, bodyFallback, localOnly });
  const activeUsers = (users ?? []).filter((user) => user.disabled_at === null);
  const userOptions = [
    { value: "", label: t("settings.emailUserAuto") },
    ...activeUsers.map((user) => ({ value: user.id, label: user.display_name })),
  ];

  const statusLabel = data
    ? data.source === "env"
      ? t("settings.emailStatusEnv")
      : data.enabled
        ? t("settings.emailStatusActive")
        : t("settings.emailStatusDisabled")
    : null;
  const statusClass = data?.enabled ? "settings-badge" : "settings-badge settings-badge--plain";

  async function toggleEnabled(enabled: boolean) {
    setBusy(true);
    onError(null);
    try {
      const updated = await api.updateEmailSettings({ enabled });
      queryClient.setQueryData(emailSettingsQuery.queryKey, updated);
      toast(t(enabled ? "settings.emailEnabledToast" : "settings.emailDisabledToast"), { icon: "check" });
    } catch (cause) {
      onError(errorCode(cause));
    } finally {
      setBusy(false);
    }
  }

  async function generateOrRotate(rotated: boolean) {
    if (rotated) {
      const ok = await confirm({
        title: t("settings.emailRotateTitle"),
        description: t("settings.emailRotateConfirm"),
        confirmLabel: t("settings.emailRotateSecret"),
        cancelLabel: t("common.cancel"),
        destructive: true,
      });
      if (!ok) return;
    }
    setBusy(true);
    onError(null);
    setCopied(null);
    try {
      const created = await api.rotateEmailSecret();
      setSecret(created.secret);
      queryClient.setQueryData(emailSettingsQuery.queryKey, created.settings);
      toast(t(rotated ? "settings.emailRotatedToast" : "settings.emailSecretCreatedToast"), { icon: "check" });
    } catch (cause) {
      onError(errorCode(cause));
    } finally {
      setBusy(false);
    }
  }

  async function revokeSecret() {
    const ok = await confirm({
      title: t("settings.emailRevokeTitle"),
      description: t("settings.emailRevokeConfirm"),
      confirmLabel: t("settings.revoke"),
      cancelLabel: t("common.cancel"),
      destructive: true,
    });
    if (!ok) return;

    setBusy(true);
    onError(null);
    setCopied(null);
    try {
      const updated = await api.revokeEmailSecret();
      setSecret(null);
      queryClient.setQueryData(emailSettingsQuery.queryKey, updated);
      toast(t("settings.emailRevokedToast"), { icon: "check" });
    } catch (cause) {
      onError(errorCode(cause));
    } finally {
      setBusy(false);
    }
  }

  async function copyValue(value: string, input: HTMLInputElement | null, kind: "secret" | "endpoint", toastKey: string) {
    if (!value) return;
    try {
      await navigator.clipboard.writeText(value);
      setCopied(kind);
      toast(t(toastKey), { icon: "check" });
    } catch {
      input?.select();
      onError("clipboard");
    }
  }

  async function saveSettings(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    onError(null);
    const patch: EmailSettingsPatch = {
      allowed_senders: parsedSenders,
      user_id: userId || null,
      tag: tag.trim() || null,
      body_fallback: bodyFallback,
      local_only: localOnly,
    };
    try {
      const updated = await api.updateEmailSettings(patch);
      queryClient.setQueryData(emailSettingsQuery.queryKey, updated);
      setPrevData(updated);
      setAllowedSendersText(updated.allowed_senders.join("\n"));
      setUserId(updated.user_id ?? "");
      setTag(updated.tag ?? "");
      setBodyFallback(updated.body_fallback);
      setLocalOnly(updated.local_only);
      toast(t("settings.emailSavedToast"), { icon: "check" });
    } catch (cause) {
      onError(errorCode(cause));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="settings-section" aria-labelledby="settings-email-intake">
      <div className="settings-section-heading">
        <div>
          <h2 id="settings-email-intake">{t("settings.emailIntake")}</h2>
          <p>{t("settings.emailIntakeDescription")}</p>
        </div>
        {statusLabel ? <span className={statusClass}>{statusLabel}</span> : null}
      </div>

      {data ? (
        <div className="email-intake-stack">
          <label className="toggle-row">
            <span className="toggle-row-text">
              <Mail aria-hidden="true" />
              <span>
                <strong>{t("settings.emailEnabled")}</strong>
                <small>{data.has_secret ? t("settings.emailEnabledHint") : t("settings.emailSecretRequiredHint")}</small>
              </span>
            </span>
            <input
              type="checkbox"
              role="switch"
              className="switch"
              checked={data.enabled}
              disabled={busy || !data.has_secret}
              onChange={(event) => void toggleEnabled(event.target.checked)}
              aria-label={t("settings.emailEnabled")}
            />
          </label>

          {secret ? (
            <div className="token-secret" role="status">
              <div>
                <strong>{t("settings.emailSecretTitle")}</strong>
                <p>{t("settings.emailSecretDescription")}</p>
              </div>
              <div className="token-secret-row">
                <input
                  ref={secretInput}
                  readOnly
                  value={secret}
                  aria-label={t("settings.emailSecretLabel")}
                  onFocus={(event) => event.currentTarget.select()}
                />
                <button
                  type="button"
                  className="button button--secondary"
                  aria-label={t("settings.emailCopySecret")}
                  onClick={() => void copyValue(secret, secretInput.current, "secret", "settings.emailSecretCopiedToast")}
                >
                  {copied === "secret" ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
                  {copied === "secret" ? t("document.copied") : t("settings.emailCopySecret")}
                </button>
              </div>
            </div>
          ) : null}

          {data.has_secret ? (
            <ul className="settings-records token-records">
              <li>
                <div>
                  <strong>
                    {data.secret_source === "env"
                      ? t("settings.emailSecretEnv")
                      : data.secret_prefix
                        ? <code>{data.secret_prefix}…</code>
                        : t("settings.emailSecretConfigured")}
                  </strong>
                  <span>
                    {t("settings.emailLastReceived", { date: formatDate(data.last_received_at, language, t("settings.never")) })}
                    {" · "}
                    {t("settings.emailReceivedCount", { count: data.received_count })}
                  </span>
                </div>
                <div className="settings-record-actions">
                  <button
                    type="button"
                    className="quiet-button"
                    disabled={busy}
                    onClick={() => void generateOrRotate(true)}
                    aria-label={t("settings.emailRotateSecret")}
                  >
                    <RefreshCw aria-hidden="true" />{t("settings.emailRotateSecret")}
                  </button>
                  <button
                    type="button"
                    className="quiet-button settings-action-danger"
                    disabled={busy}
                    onClick={() => void revokeSecret()}
                    aria-label={t("settings.emailRevokeSecret")}
                  >
                    <Trash2 aria-hidden="true" />{t("settings.revoke")}
                  </button>
                </div>
              </li>
            </ul>
          ) : (
            <div className="email-secret-empty">
              <p className="settings-note">{t("settings.emailSecretEmpty")}</p>
              <button
                type="button"
                className="button button--secondary"
                disabled={busy}
                onClick={() => void generateOrRotate(false)}
              >
                <Plus aria-hidden="true" />{t("settings.emailGenerateSecret")}
              </button>
            </div>
          )}

          <div className="email-endpoint-field">
            <label className="field-label" htmlFor={endpointId}>
              <span>{t("settings.emailEndpointTitle")}</span>
            </label>
            <div className="token-secret-row">
              <input
                id={endpointId}
                ref={endpointInput}
                readOnly
                value={data.endpoint_url}
                aria-label={t("settings.emailEndpointLabel")}
                onFocus={(event) => event.currentTarget.select()}
              />
              <button
                type="button"
                className="button button--secondary"
                aria-label={t("settings.emailCopyEndpoint")}
                onClick={() => void copyValue(data.endpoint_url, endpointInput.current, "endpoint", "settings.emailEndpointCopiedToast")}
              >
                {copied === "endpoint" ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
                {copied === "endpoint" ? t("document.copied") : t("settings.emailCopyEndpoint")}
              </button>
            </div>
          </div>

          <details className="processing-details email-worker-details">
            <summary>
              <span>{t("settings.emailWorkerSummary")}</span>
              <span>{t("settings.emailWorkerAction")}<ChevronDown className="disclosure-chevron" aria-hidden="true" /></span>
            </summary>
            <div className="email-worker-body">
              <p className="settings-note">{t("settings.emailWorkerIntro")}</p>
              <dl>
                <div>
                  <dt><code>PAPERJAM_URL</code></dt>
                  <dd>{t("settings.emailWorkerVarUrl")}</dd>
                </div>
                <div>
                  <dt><code>EMAIL_SECRET</code></dt>
                  <dd>{t("settings.emailWorkerVarSecret")}</dd>
                </div>
                <div>
                  <dt><code>ALLOWED_SENDERS</code></dt>
                  <dd>{t("settings.emailWorkerVarSenders")}</dd>
                </div>
              </dl>
              <p className="settings-note">
                {t("settings.emailWorkerEndpointNote")} <code>POST /api/v1/ingest/email</code>
              </p>
            </div>
          </details>

          <form className="email-intake-form" onSubmit={saveSettings}>
            <label className="field-label" htmlFor={sendersId}>
              <span>{t("settings.emailAllowedSenders")}</span>
              <textarea
                id={sendersId}
                rows={3}
                value={allowedSendersText}
                onChange={(event) => setAllowedSendersText(event.target.value)}
                placeholder={t("settings.emailAllowedSendersPlaceholder")}
                aria-label={t("settings.emailAllowedSenders")}
                aria-describedby={sendersHintId}
                autoCorrect="off"
                autoCapitalize="none"
                spellCheck={false}
              />
            </label>
            {parsedSenders.length === 0 ? (
              <p id={sendersHintId} className="email-field-hint email-field-hint--notice">
                <Info aria-hidden="true" />
                <span>{t("settings.emailAllowedSendersEmptyNote")}</span>
              </p>
            ) : (
              <p id={sendersHintId} className="email-field-hint">
                {t("settings.emailAllowedSendersHint")}
              </p>
            )}

            <div className="settings-form-grid">
              <div className="field-label">
                <span>{t("settings.emailTargetUser")}</span>
                <SelectDropdown
                  value={userId}
                  onChange={setUserId}
                  placeholder={t("settings.emailUserAuto")}
                  aria-label={t("settings.emailTargetUser")}
                  options={userOptions}
                />
              </div>
              <label className="field-label" htmlFor={tagId}>
                <span>{t("settings.emailTag")}</span>
                <input
                  id={tagId}
                  maxLength={64}
                  autoComplete="off"
                  autoCorrect="off"
                  autoCapitalize="none"
                  spellCheck={false}
                  placeholder={t("settings.emailTagPlaceholder")}
                  value={tag}
                  onChange={(event) => setTag(event.target.value)}
                  aria-label={t("settings.emailTag")}
                />
              </label>
            </div>

            <label className="toggle-row">
              <span className="toggle-row-text">
                <FileText aria-hidden="true" />
                <span>
                  <strong>{t("settings.emailBodyFallback")}</strong>
                  <small>{t("settings.emailBodyFallbackDescription")}</small>
                </span>
              </span>
              <input
                type="checkbox"
                role="switch"
                className="switch"
                checked={bodyFallback}
                onChange={(event) => setBodyFallback(event.target.checked)}
                aria-label={t("settings.emailBodyFallback")}
              />
            </label>

            <label className="toggle-row">
              <span className="toggle-row-text">
                <CloudOff aria-hidden="true" />
                <span>
                  <strong>{t("settings.emailLocalOnly")}</strong>
                  <small>{t("settings.emailLocalOnlyDescription")}</small>
                </span>
              </span>
              <input
                type="checkbox"
                role="switch"
                className="switch"
                checked={localOnly}
                onChange={(event) => setLocalOnly(event.target.checked)}
                aria-label={t("settings.emailLocalOnly")}
              />
            </label>

            <div className="settings-form-footer">
              <div>
                {dirty ? <span className="taxonomy-dirty" role="status">{t("taxonomy.unsaved")}</span> : null}
              </div>
              <div className="settings-form-actions">
                <button type="submit" className="button button--primary" disabled={busy || !dirty}>
                  {t("settings.emailSave")}
                </button>
              </div>
            </div>
          </form>
        </div>
      ) : null}
    </section>
  );
}
