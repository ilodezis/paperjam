import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Archive, Ban, Check, ChevronRight, Copy, KeyRound, Lock, Monitor, Moon, Pencil, Plus, RotateCcw, SlidersHorizontal, Sun, Trash2, Unplug, UserPlus } from "lucide-react";
import { useEffect, useRef, useState, type ComponentType, type FormEvent, type KeyboardEvent } from "react";
import { useTranslation } from "react-i18next";

import { ApiError, api, type Connection, type ManagedUser, type TokenView } from "../api";
import { DocumentTypesSettings } from "../components/DocumentTypesSettings";
import { CalendarFeedSection } from "../components/CalendarFeedSection";
import { PasskeysSection } from "../components/PasskeysSection";
import { useConfirm } from "../components/controls/ConfirmDialog";
import { SegmentedControl } from "../components/controls/SegmentedControl";
import { toast } from "../components/controls/toast";
import { connectionsQuery, sessionQuery, statsQuery, tokensQuery, usersQuery } from "../queries";
import { hrefOf, ScreenLink } from "../shell/navigation";
import { TopBar } from "../shell/PageHead";
import { setThemePreference, useThemePreference } from "../theme";

type TabId = "interface" | "archive" | "access";
type ReportError = (code: string | null) => void;
type Confirm = ReturnType<typeof useConfirm>["confirm"];

const TABS: { id: TabId; icon: ComponentType<{ "aria-hidden"?: boolean | "true" | "false" }> }[] = [
  { id: "interface", icon: SlidersHorizontal },
  { id: "archive", icon: Archive },
  { id: "access", icon: KeyRound },
];

function formatDate(value: string | null, language: string, never: string): string {
  return value ? new Intl.DateTimeFormat(language, { dateStyle: "medium" }).format(new Date(value)) : never;
}

function errorCode(cause: unknown): string {
  return cause instanceof ApiError ? cause.code : "network";
}

/** Runs focus() on `target` when `open` goes from true to false, so closing a form hands focus back to the button that opened it. */
function useRestoreFocus(open: boolean, target: { current: HTMLElement | null }) {
  const wasOpen = useRef(open);
  useEffect(() => {
    if (wasOpen.current && !open) target.current?.focus();
    wasOpen.current = open;
  }, [open, target]);
}

export function SettingsPage() {
  const { t } = useTranslation();
  const { confirm, dialogElement } = useConfirm();
  const { data: users } = useQuery(usersQuery);
  const [tab, setTab] = useState<TabId>("interface");
  const [error, setError] = useState<string | null>(null);
  const tabRefs = useRef<Record<TabId, HTMLButtonElement | null>>({ interface: null, archive: null, access: null });

  // Arrow keys move between tabs and select as they go, as the tab pattern prescribes.
  function onTabKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    const index = TABS.findIndex(({ id }) => id === tab);
    const next = event.key === "ArrowRight" ? (index + 1) % TABS.length
      : event.key === "ArrowLeft" ? (index + TABS.length - 1) % TABS.length
      : event.key === "Home" ? 0
      : event.key === "End" ? TABS.length - 1
      : null;
    if (next === null) return;
    event.preventDefault();
    setTab(TABS[next].id);
    tabRefs.current[TABS[next].id]?.focus();
  }

  return (
    <div className="settings-page">
      <TopBar title={t("settings.title")} />
      <header className="management-header"><div><h1>{t("settings.title")}</h1></div></header>
      <ScreenLink to={hrefOf("trash")} className="settings-trash-link">
        <Trash2 aria-hidden="true" /><span>{t("nav.trash")}</span><ChevronRight aria-hidden="true" />
      </ScreenLink>
      {error ? <div className="settings-error" role="alert">{t(`errors.${error}`, { defaultValue: t("errors.settings") })}<button type="button" className="quiet-button" onClick={() => setError(null)}>{t("common.dismiss")}</button></div> : null}

      <div className="settings-tabs" role="tablist" aria-label={t("settings.sections")} onKeyDown={onTabKeyDown}>
        {TABS.map(({ id, icon: Icon }) => (
          <button
            key={id}
            ref={(node) => { tabRefs.current[id] = node; }}
            type="button"
            role="tab"
            id={`settings-tab-${id}`}
            aria-selected={tab === id}
            aria-controls={`settings-panel-${id}`}
            tabIndex={tab === id ? 0 : -1}
            className={tab === id ? "settings-tab settings-tab--active" : "settings-tab"}
            onClick={() => setTab(id)}
          >
            <Icon aria-hidden="true" />{t(`settings.tabs.${id}`)}
          </button>
        ))}
      </div>

      {/* Every panel stays mounted: a token secret that is shown only once, or a half-filled form, must survive a switch of tab. */}
      <div role="tabpanel" id="settings-panel-interface" aria-labelledby="settings-tab-interface" className="settings-panel" hidden={tab !== "interface"}>
        <InterfaceSettings onError={setError} />
      </div>
      <div role="tabpanel" id="settings-panel-archive" aria-labelledby="settings-tab-archive" className="settings-panel" hidden={tab !== "archive"}>
        <ArchiveSettings />
      </div>
      <div role="tabpanel" id="settings-panel-access" aria-labelledby="settings-tab-access" className="settings-panel" hidden={tab !== "access"}>
        <AccessSettings admin={Boolean(users)} confirm={confirm} onError={setError} />
      </div>
      {dialogElement}
    </div>
  );
}

/** Personal to this person and device: theme and language. Nothing administrative here. */
function InterfaceSettings({ onError }: { onError: ReportError }) {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();
  const theme = useThemePreference();
  const language = i18n.resolvedLanguage ?? "en";

  function changeLanguage(value: string) {
    void i18n.changeLanguage(value);
    window.localStorage.setItem("paperjam:language", value);
    document.documentElement.lang = value;
    // The account keeps the language as well: titles and summaries of the
    // documents this user adds are written in it.
    api.setLocale(value)
      .then((session) => queryClient.setQueryData(sessionQuery.queryKey, session))
      .catch((cause: unknown) => onError(errorCode(cause)));
  }

  return (
    <section className="settings-section settings-interface" aria-labelledby="settings-tab-interface">
      <div className="settings-interface-grid">
        <div className="settings-control-group">
          <span className="control-group-label">{t("preferences.theme")}</span>
          <SegmentedControl
            value={theme}
            onChange={setThemePreference}
            ariaLabel={t("preferences.theme")}
            options={[
              { value: "system", label: t("preferences.system"), icon: Monitor },
              { value: "light", label: t("preferences.light"), icon: Sun },
              { value: "dark", label: t("preferences.dark"), icon: Moon },
            ]}
          />
        </div>
        <div className="settings-control-group">
          <span className="control-group-label">{t("preferences.language")}</span>
          <SegmentedControl<string>
            value={language}
            onChange={changeLanguage}
            ariaLabel={t("settings.interfaceLanguage")}
            options={[
              { value: "ru", label: "Русский" },
              { value: "en", label: "English" },
            ]}
          />
        </div>
      </div>
      <p className="settings-note">{t("settings.languageDescription")}</p>
    </section>
  );
}

/** Archive usage and the advanced document schemas. */
function ArchiveSettings() {
  const { t, i18n } = useTranslation();
  const { data: stats } = useQuery(statsQuery);
  const language = i18n.resolvedLanguage ?? "en";
  const spend = new Intl.NumberFormat(language, { style: "currency", currency: "USD", minimumFractionDigits: 2, maximumFractionDigits: 2 }).format(Number(stats?.llm_month_usd ?? 0));

  return (
    <>
      <section className="settings-section settings-usage" aria-labelledby="settings-usage">
        <div className="settings-section-heading"><div><h2 id="settings-usage">{t("settings.processing")}</h2></div></div>
        <dl className="usage-list"><div><dt>{t("settings.monthSpend")}</dt><dd>{spend}</dd></div><div><dt>{t("settings.documents")}</dt><dd>{stats?.documents ?? "—"}</dd></div><div><dt>{t("settings.attention")}</dt><dd>{stats?.attention ?? "—"}</dd></div></dl>
        <h3>{t("settings.queue")}</h3>
        <dl className="queue-list"><div><dt>{t("settings.queued")}</dt><dd>{stats?.queue.queued ?? "—"}</dd></div><div><dt>{t("settings.running")}</dt><dd>{stats?.queue.running ?? "—"}</dd></div><div><dt>{t("settings.dead")}</dt><dd>{stats?.queue.dead ?? "—"}</dd></div></dl>
      </section>
      <DocumentTypesSettings />
    </>
  );
}

/** Who and what can get in: users (administrators only), password, API tokens, the calendar link, connected apps, passkeys. */
function AccessSettings({ admin, confirm, onError }: { admin: boolean; confirm: Confirm; onError: ReportError }) {
  return (
    <>
      {admin ? <UsersSection confirm={confirm} onError={onError} /> : null}
      <PasswordSection onError={onError} />
      <TokensSection confirm={confirm} onError={onError} />
      <CalendarFeedSection confirm={confirm} onError={onError} />
      <ConnectionsSection confirm={confirm} onError={onError} />
      <PasskeysSection />
    </>
  );
}

function UsersSection({ confirm, onError }: { confirm: Confirm; onError: ReportError }) {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();
  const { data: users } = useQuery(usersQuery);
  const { data: session } = useQuery(sessionQuery);
  const currentUserId = session?.user?.id;
  const language = i18n.resolvedLanguage ?? "en";
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [username, setUsername] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [password, setPassword] = useState("");
  const [admin, setAdmin] = useState(false);
  const [resetTarget, setResetTarget] = useState<ManagedUser | null>(null);
  const [resetPasswordValue, setResetPasswordValue] = useState("");
  const [editTarget, setEditTarget] = useState<ManagedUser | null>(null);
  const [editDisplayName, setEditDisplayName] = useState("");
  const [editAdmin, setEditAdmin] = useState(false);
  const opener = useRef<HTMLButtonElement>(null);
  useRestoreFocus(open, opener);

  function close() {
    setOpen(false);
    setUsername("");
    setDisplayName("");
    setPassword("");
    setAdmin(false);
  }

  function closeReset() {
    setResetTarget(null);
    setResetPasswordValue("");
  }

  function closeEdit() {
    setEditTarget(null);
    setEditDisplayName("");
    setEditAdmin(false);
  }

  function startEdit(user: ManagedUser) {
    close();
    closeReset();
    setEditTarget(user);
    setEditDisplayName(user.display_name);
    setEditAdmin(user.is_admin);
  }

  function startReset(user: ManagedUser) {
    close();
    closeEdit();
    setResetTarget(user);
    setResetPasswordValue("");
  }

  async function createUser(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    onError(null);
    try {
      await api.createUser({ username, display_name: displayName || undefined, password, locale: language, is_admin: admin });
      await queryClient.invalidateQueries({ queryKey: ["users"] });
      await queryClient.invalidateQueries({ queryKey: ["stats"] });
      close();
      toast(t("settings.userCreatedToast"), { icon: "check" });
    } catch (cause) {
      onError(errorCode(cause));
    } finally {
      setBusy(false);
    }
  }

  async function toggleDisabled(user: ManagedUser) {
    const disabling = user.disabled_at === null;
    if (disabling) {
      const ok = await confirm({
        title: t("settings.disableUserFor", { name: user.display_name }),
        description: t("settings.disableUserConfirm", { name: user.display_name }),
        confirmLabel: t("settings.disableUser"),
        destructive: true,
      });
      if (!ok) return;
    }

    setBusy(true);
    onError(null);
    try {
      await api.updateUser(user.id, { disabled: disabling });
      await queryClient.invalidateQueries({ queryKey: ["users"] });
      toast(t(disabling ? "settings.userDisabledToast" : "settings.userEnabledToast"), { icon: "check" });
    } catch (cause) {
      onError(errorCode(cause));
    } finally {
      setBusy(false);
    }
  }

  async function removeUser(user: ManagedUser) {
    const ok = await confirm({
      title: t("settings.deleteUserFor", { name: user.display_name }),
      description: t("settings.deleteUserConfirm", { name: user.display_name }),
      confirmLabel: t("settings.deleteUser"),
      destructive: true,
    });
    if (!ok) return;

    setBusy(true);
    onError(null);
    try {
      await api.deleteUser(user.id);
      if (resetTarget?.id === user.id) closeReset();
      if (editTarget?.id === user.id) closeEdit();
      await queryClient.invalidateQueries({ queryKey: ["users"] });
      toast(t("settings.userDeletedToast"), { icon: "check" });
    } catch (cause) {
      onError(errorCode(cause));
    } finally {
      setBusy(false);
    }
  }

  async function submitResetPassword(event: FormEvent) {
    event.preventDefault();
    if (!resetTarget) return;
    setBusy(true);
    onError(null);
    try {
      await api.updateUser(resetTarget.id, { password: resetPasswordValue });
      closeReset();
      toast(t("settings.userPasswordResetToast"), { icon: "check" });
    } catch (cause) {
      onError(errorCode(cause));
    } finally {
      setBusy(false);
    }
  }

  async function submitEditUser(event: FormEvent) {
    event.preventDefault();
    if (!editTarget) return;
    setBusy(true);
    onError(null);
    try {
      await api.updateUser(editTarget.id, { display_name: editDisplayName.trim(), is_admin: editAdmin });
      await queryClient.invalidateQueries({ queryKey: ["users"] });
      closeEdit();
      toast(t("settings.userUpdatedToast"), { icon: "check" });
    } catch (cause) {
      onError(errorCode(cause));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="settings-section" aria-labelledby="settings-users">
      <div className="settings-section-heading">
        <div><h2 id="settings-users">{t("settings.users")}</h2></div>
        {open ? null : <button ref={opener} type="button" className="button button--secondary" aria-expanded="false" aria-controls="settings-user-form" onClick={() => { closeReset(); closeEdit(); setOpen(true); }}><UserPlus aria-hidden="true" />{t("settings.addUser")}</button>}
      </div>
      {open ? (
        <form id="settings-user-form" className="settings-create-form" onSubmit={createUser}>
          <div className="settings-form-grid">
            <label className="field-label"><span>{t("settings.username")}</span><input required autoFocus pattern="[a-z0-9_-]+" autoComplete="off" value={username} onChange={(event) => setUsername(event.target.value)} aria-label={t("settings.newUsername")} /></label>
            <label className="field-label"><span>{t("settings.displayName")}</span><input value={displayName} onChange={(event) => setDisplayName(event.target.value)} aria-label={t("settings.newDisplayName")} /></label>
            <label className="field-label"><span>{t("settings.password")}</span><input required minLength={8} type="password" autoComplete="new-password" value={password} onChange={(event) => setPassword(event.target.value)} aria-label={t("settings.newPassword")} /></label>
          </div>
          <div className="settings-form-footer">
            <label className="required-control"><input type="checkbox" checked={admin} onChange={(event) => setAdmin(event.target.checked)} /><span>{t("settings.makeAdmin")}</span></label>
            <div className="settings-form-actions">
              <button type="button" className="button button--secondary" onClick={close}>{t("common.cancel")}</button>
              <button type="submit" className="button button--primary" disabled={busy}><Plus aria-hidden="true" />{t("settings.createUser")}</button>
            </div>
          </div>
        </form>
      ) : null}
      {editTarget ? (
        <form id="settings-edit-user-form" className="settings-create-form settings-inline-panel" onSubmit={submitEditUser}>
          <div className="settings-form-grid">
            <label className="field-label">
              <span>{t("settings.displayName")}</span>
              <input
                required
                autoFocus
                maxLength={100}
                value={editDisplayName}
                onChange={(event) => setEditDisplayName(event.target.value)}
                aria-label={t("settings.editDisplayNameFor", { name: editTarget.display_name })}
              />
            </label>
          </div>
          <div className="settings-form-footer">
            <label className="required-control">
              <input
                type="checkbox"
                checked={editAdmin}
                onChange={(event) => setEditAdmin(event.target.checked)}
                aria-label={t("settings.editAdminFor", { name: editTarget.display_name })}
              />
              <span>{t("settings.makeAdmin")}</span>
            </label>
            <div className="settings-form-actions">
              <button type="button" className="button button--secondary" onClick={closeEdit}>{t("common.cancel")}</button>
              <button type="submit" className="button button--primary" disabled={busy || !editDisplayName.trim()}>{t("settings.saveUser")}</button>
            </div>
          </div>
        </form>
      ) : null}
      {resetTarget ? (
        <form id="settings-reset-password-form" className="token-create settings-inline-panel" onSubmit={submitResetPassword}>
          <label className="sr-only" htmlFor="reset-user-password">{t("settings.resetPasswordFor", { name: resetTarget.display_name })}</label>
          <input
            id="reset-user-password"
            required
            autoFocus
            minLength={8}
            type="password"
            autoComplete="new-password"
            placeholder={t("settings.resetPasswordPlaceholder")}
            value={resetPasswordValue}
            onChange={(event) => setResetPasswordValue(event.target.value)}
            aria-label={t("settings.resetPasswordFor", { name: resetTarget.display_name })}
          />
          <button type="submit" className="button button--primary" disabled={busy || resetPasswordValue.length < 8}>{t("settings.savePassword")}</button>
          <button type="button" className="button button--secondary" onClick={closeReset}>{t("common.cancel")}</button>
        </form>
      ) : null}
      <ul className="settings-records">
        {users?.map((user) => {
          const disabled = user.disabled_at !== null;
          const isSelf = user.id === currentUserId;
          return (
            <li key={user.id} className={disabled ? "token-revoked" : ""}>
              <div>
                <strong>{user.display_name}</strong>
                {user.username !== user.display_name ? <span>{user.username}</span> : null}
              </div>
              <div className="settings-record-actions">
                {disabled ? <span className="settings-badge settings-badge--plain">{t("settings.disabled")}</span> : null}
                <span className={user.is_admin ? "settings-badge" : "settings-badge settings-badge--plain"}>{user.is_admin ? t("settings.administrator") : t("settings.member")}</span>
                {isSelf ? null : (
                  <>
                    <button type="button" className="quiet-button" disabled={busy} onClick={() => startEdit(user)} aria-label={t("settings.editUserFor", { name: user.display_name })}><Pencil aria-hidden="true" />{t("settings.editUser")}</button>
                    <button type="button" className="quiet-button" disabled={busy} onClick={() => startReset(user)} aria-label={t("settings.resetPasswordFor", { name: user.display_name })}><KeyRound aria-hidden="true" />{t("settings.resetPassword")}</button>
                    <button type="button" className="quiet-button" disabled={busy} onClick={() => void toggleDisabled(user)} aria-label={t(disabled ? "settings.enableUserFor" : "settings.disableUserFor", { name: user.display_name })}>{disabled ? <RotateCcw aria-hidden="true" /> : <Ban aria-hidden="true" />}{t(disabled ? "settings.enableUser" : "settings.disableUser")}</button>
                    <button type="button" className="quiet-button settings-action-danger" disabled={busy} onClick={() => void removeUser(user)} aria-label={t("settings.deleteUserFor", { name: user.display_name })}><Trash2 aria-hidden="true" />{t("settings.deleteUser")}</button>
                  </>
                )}
              </div>
            </li>
          );
        })}
      </ul>
    </section>
  );
}

function PasswordSection({ onError }: { onError: ReportError }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [currentPassword, setCurrentPassword] = useState("");
  const [newPassword, setNewPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");
  const opener = useRef<HTMLButtonElement>(null);
  useRestoreFocus(open, opener);

  function close() {
    setOpen(false);
    setCurrentPassword("");
    setNewPassword("");
    setConfirmPassword("");
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (newPassword !== confirmPassword) {
      onError("password_mismatch");
      return;
    }
    setBusy(true);
    onError(null);
    try {
      await api.changePassword(currentPassword, newPassword);
      close();
      toast(t("settings.passwordChangedToast"), { icon: "check" });
    } catch (cause) {
      onError(errorCode(cause));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="settings-section" aria-labelledby="settings-password">
      <div className="settings-section-heading">
        <div>
          <h2 id="settings-password">{t("settings.passwordSection")}</h2>
          <p>{t("settings.passwordDescription")}</p>
        </div>
        {open ? null : <button ref={opener} type="button" className="button button--secondary" aria-expanded="false" aria-controls="settings-password-form" onClick={() => setOpen(true)}><Lock aria-hidden="true" />{t("settings.changePassword")}</button>}
      </div>
      {open ? (
        <form id="settings-password-form" className="settings-create-form" onSubmit={submit}>
          <div className="settings-form-grid">
            <label className="field-label"><span>{t("settings.currentPassword")}</span><input required autoFocus type="password" autoComplete="current-password" value={currentPassword} onChange={(event) => setCurrentPassword(event.target.value)} aria-label={t("settings.currentPassword")} /></label>
            <label className="field-label"><span>{t("settings.newAccountPassword")}</span><input required minLength={8} type="password" autoComplete="new-password" value={newPassword} onChange={(event) => setNewPassword(event.target.value)} aria-label={t("settings.newAccountPassword")} /></label>
            <label className="field-label"><span>{t("settings.confirmPassword")}</span><input required minLength={8} type="password" autoComplete="new-password" value={confirmPassword} onChange={(event) => setConfirmPassword(event.target.value)} aria-label={t("settings.confirmPassword")} /></label>
          </div>
          <div className="settings-form-footer">
            <div />
            <div className="settings-form-actions">
              <button type="button" className="button button--secondary" onClick={close}>{t("common.cancel")}</button>
              <button type="submit" className="button button--primary" disabled={busy}>{t("settings.savePassword")}</button>
            </div>
          </div>
        </form>
      ) : null}
    </section>
  );
}

function TokensSection({ confirm, onError }: { confirm: Confirm; onError: ReportError }) {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();
  // "Last used" moves whenever a token is used elsewhere, so the list is fetched again each time this tab opens.
  const { data: tokens } = useQuery({ ...tokensQuery, refetchOnMount: "always" });
  const language = i18n.resolvedLanguage ?? "en";
  const secretInput = useRef<HTMLInputElement>(null);
  const opener = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const [secret, setSecret] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [busy, setBusy] = useState(false);
  const [tokenName, setTokenName] = useState("");
  useRestoreFocus(open, opener);

  function close() {
    setOpen(false);
    setTokenName("");
  }

  async function createToken(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    onError(null);
    setCopied(false);
    try {
      const created = await api.createToken(tokenName);
      await queryClient.invalidateQueries({ queryKey: ["tokens"] });
      setSecret(created.secret);
      close();
      toast(t("settings.tokenCreatedToast"), { icon: "check" });
    } catch (cause) {
      onError(errorCode(cause));
    } finally {
      setBusy(false);
    }
  }

  async function copySecret() {
    if (!secret) return;
    try {
      await navigator.clipboard.writeText(secret);
      setCopied(true);
      toast(t("settings.tokenCopiedToast"), { icon: "check" });
    } catch {
      // Some browsers refuse clipboard access; the selected text can still be copied by hand.
      secretInput.current?.select();
      onError("clipboard");
    }
  }

  async function revoke(token: TokenView) {
    const ok = await confirm({
      title: t("settings.revokeFor", { name: token.name }),
      description: t("settings.revokeConfirm", { name: token.name }),
      confirmLabel: t("settings.revoke"),
      destructive: true,
    });
    if (!ok) return;

    setBusy(true);
    onError(null);
    try {
      await api.revokeToken(token.id);
      await queryClient.invalidateQueries({ queryKey: ["tokens"] });
      toast(t("settings.tokenRevokedToast"), { icon: "check" });
    } catch (cause) {
      onError(errorCode(cause));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="settings-section" aria-labelledby="settings-tokens">
      <div className="settings-section-heading">
        <div><h2 id="settings-tokens">{t("settings.tokens")}</h2><p>{t("settings.tokensDescription")}</p></div>
        {open ? null : <button ref={opener} type="button" className="button button--secondary" aria-expanded="false" aria-controls="settings-token-form" onClick={() => setOpen(true)}><Plus aria-hidden="true" />{t("settings.newToken")}</button>}
      </div>
      {secret ? <div className="token-secret" role="status"><div><strong>{t("settings.secretTitle")}</strong><p>{t("settings.secretDescription")}</p></div><div className="token-secret-row"><input ref={secretInput} readOnly value={secret} aria-label={t("settings.newSecret")} onFocus={(event) => event.currentTarget.select()} /><button type="button" className="button button--secondary" onClick={() => void copySecret()}>{copied ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}{copied ? t("document.copied") : t("settings.copySecret")}</button></div></div> : null}
      {open ? (
        <form id="settings-token-form" className="token-create" onSubmit={createToken}>
          <label className="sr-only" htmlFor="token-name">{t("settings.newTokenName")}</label>
          <input id="token-name" required autoFocus maxLength={100} autoComplete="off" placeholder={t("settings.tokenPlaceholder")} value={tokenName} onChange={(event) => setTokenName(event.target.value)} aria-label={t("settings.newTokenName")} />
          <button type="submit" className="button button--primary" disabled={busy || !tokenName.trim()}><Plus aria-hidden="true" />{t("settings.createToken")}</button>
          <button type="button" className="button button--secondary" onClick={close}>{t("common.cancel")}</button>
        </form>
      ) : null}
      <ul className="settings-records token-records">
        {tokens?.map((token) => <li key={token.id} className={token.revoked_at ? "token-revoked" : ""}><div><strong>{token.name}</strong><span><code>{token.prefix}…</code> · {t("settings.created", { date: formatDate(token.created_at, language, t("settings.never")) })}</span><span>{t("settings.lastUsed", { date: formatDate(token.last_used_at, language, t("settings.never")) })}</span></div>{token.revoked_at ? <span className="settings-badge settings-badge--plain">{t("settings.revoked")}</span> : <button type="button" className="quiet-button" disabled={busy} onClick={() => void revoke(token)} aria-label={t("settings.revokeFor", { name: token.name })}><Trash2 aria-hidden="true" />{t("settings.revoke")}</button>}</li>)}
      </ul>
    </section>
  );
}

function ConnectionsSection({ confirm, onError }: { confirm: Confirm; onError: ReportError }) {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();
  const { data: connections } = useQuery(connectionsQuery);
  const language = i18n.resolvedLanguage ?? "en";
  const [busy, setBusy] = useState(false);

  async function disconnect(connection: Connection) {
    const ok = await confirm({
      title: t("settings.disconnectFor", { name: connection.client_name }),
      description: t("settings.disconnectConfirm", { name: connection.client_name }),
      confirmLabel: t("settings.disconnect"),
      destructive: true,
    });
    if (!ok) return;

    setBusy(true);
    onError(null);
    try {
      await api.disconnect(connection.id);
      await queryClient.invalidateQueries({ queryKey: ["connections"] });
      toast(t("settings.disconnectedToast"), { icon: "check" });
    } catch (cause) {
      onError(errorCode(cause));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="settings-section" aria-labelledby="settings-connections">
      <div className="settings-section-heading"><div><h2 id="settings-connections">{t("settings.connections")}</h2><p>{t("settings.connectionsDescription")}</p></div></div>
      {connections && connections.length === 0 ? <p className="settings-note">{t("settings.noConnections")}</p> : null}
      {connections && connections.length > 0 ? (
        <ul className="settings-records token-records">
          {connections.map((connection) => (
            <li key={connection.id}>
              <div>
                <strong>{connection.client_name}</strong>
                <span>{t("settings.connectedOn", { date: formatDate(connection.created_at, language, t("settings.never")) })}</span>
                <span>{t("settings.lastUsed", { date: formatDate(connection.last_used_at, language, t("settings.never")) })}</span>
              </div>
              <button type="button" className="quiet-button" disabled={busy} onClick={() => void disconnect(connection)} aria-label={t("settings.disconnectFor", { name: connection.client_name })}><Unplug aria-hidden="true" />{t("settings.disconnect")}</button>
            </li>
          ))}
        </ul>
      ) : null}
    </section>
  );
}
