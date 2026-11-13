import { useId, useState, type FormEvent } from "react";
import { ArrowRight, Files } from "lucide-react";
import { useTranslation } from "react-i18next";

import { ApiError, api, type User } from "../api";
import { PaperStage, step } from "./LoginPage";
import "./login.css";

/** The first start of a new instance: the person who can read the server's log proves it with the
 *  setup code printed there, and becomes the administrator. Laid out like the sign-in it replaces. */
export function SetupPage({ onUser }: { onUser: (user: User) => void }) {
  const { t, i18n } = useTranslation();
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [reveal, setReveal] = useState(false);
  const passwordId = useId();

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setPending(true);
    setError(null);
    const form = new FormData(event.currentTarget);
    try {
      const result = await api.setup({
        code: String(form.get("code")),
        username: String(form.get("username")),
        password: String(form.get("password")),
        locale: i18n.resolvedLanguage === "ru" ? "ru" : "en",
      });
      onUser(result.user);
    } catch (cause) {
      const code = cause instanceof ApiError ? cause.code : "network";
      setError(t(`errors.${code}`, { defaultValue: t("errors.setup") }));
    } finally {
      setPending(false);
    }
  }

  const fieldClass = error ? "line-field is-error" : "line-field";
  return (
    <main className="auth">
      <div className="brand auth-brand">
        <Files aria-hidden="true" />
        <span>paperjam</span>
      </div>
      <PaperStage />
      <section className="auth-panel">
        <form className="auth-form" onSubmit={submit}>
          <div className="auth-head">
            <p className="auth-eyebrow auth-rise" style={step(0)}>{t("setup.eyebrow")}</p>
            <h1 className="auth-title auth-rise" style={step(1)}>{t("setup.title")}</h1>
            <p className="auth-lede auth-rise" style={step(2)}>{t("setup.hint")}</p>
          </div>
          <div className="auth-fields">
            <label className={`${fieldClass} auth-rise`} style={step(3)}>
              <span className="line-field-label">{t("setup.code")}</span>
              <input
                name="code"
                placeholder="xxxx-xxxx-xxxx"
                autoComplete="off"
                autoCapitalize="none"
                spellCheck={false}
                autoFocus
                required
                disabled={pending}
              />
            </label>
            <label className={`${fieldClass} auth-rise`} style={step(4)}>
              <span className="line-field-label">{t("login.username")}</span>
              <input name="username" autoComplete="username" autoCapitalize="none" spellCheck={false} required disabled={pending} />
            </label>
            <div className={`${fieldClass} auth-rise`} style={step(5)}>
              <div className="line-field-head">
                <label className="line-field-label" htmlFor={passwordId}>{t("login.password")}</label>
                <button
                  className="line-field-toggle"
                  type="button"
                  aria-controls={passwordId}
                  aria-pressed={reveal}
                  onClick={() => setReveal((shown) => !shown)}
                >
                  {reveal ? t("login.hide") : t("login.reveal")}
                </button>
              </div>
              <input
                id={passwordId}
                name="password"
                type={reveal ? "text" : "password"}
                autoComplete="new-password"
                autoCapitalize="none"
                spellCheck={false}
                minLength={8}
                required
                disabled={pending}
              />
            </div>
          </div>
          <div className="auth-foot">
            {error ? <p className="auth-error" role="alert">{error}</p> : null}
            <button className="auth-submit auth-rise" style={step(6)} type="submit" disabled={pending} aria-busy={pending}>
              <span>{pending ? t("setup.pending") : t("setup.submit")}</span>
              <ArrowRight aria-hidden="true" />
              {pending ? <span className="auth-sweep" aria-hidden="true" /> : null}
            </button>
          </div>
        </form>
      </section>
    </main>
  );
}
