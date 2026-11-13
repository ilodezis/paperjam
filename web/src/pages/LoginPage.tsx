import { useCallback, useEffect, useId, useState, type CSSProperties, type FormEvent } from "react";
import { ArrowRight, Check, Files, KeyRound } from "lucide-react";
import { useTranslation } from "react-i18next";

import { ApiError, api, type User } from "../api";
import { PasskeyLogin } from "../passkeyLogin";
import { passkeysSupported } from "../passkeys";
import "./login.css";

/** Passkey failures with a message of their own; anything else is a generic retry. */
const PASSKEY_ERRORS = new Set(["unknown_passkey", "account_disabled", "too_many_attempts", "network"]);

const CONSENT_PATH = "/oauth/authorize";

/** Where to go once signed in, when the sign-in interrupted an application asking to connect.
 *  Only this origin's consent page qualifies: anything else in `next` would make the sign-in
 *  page an open redirect. */
export function consentReturn(href: string): string | null {
  let here: URL;
  try {
    here = new URL(href);
  } catch {
    return null;
  }
  const next = here.searchParams.get("next");
  if (!next || !next.startsWith("/") || next.startsWith("//")) return null;
  let target: URL;
  try {
    target = new URL(next, here.origin);
  } catch {
    return null;
  }
  if (target.origin !== here.origin || target.pathname !== CONSENT_PATH) return null;
  return target.pathname + target.search;
}

/** Staggered entrance order for one element of the form. */
export const step = (index: number) => ({ "--i": index }) as CSSProperties;

const leavePage = (path: string) => window.location.replace(path);

export function LoginPage({ onUser, leave = leavePage }: { onUser: (user: User) => void; leave?: (path: string) => void }) {
  const { t } = useTranslation();
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [supported] = useState(passkeysSupported);
  const [passkeys, setPasskeys] = useState(false);
  const [waiting, setWaiting] = useState(false);
  const [reveal, setReveal] = useState(false);
  const [login] = useState(() => new PasskeyLogin());
  const [consent] = useState(() => consentReturn(window.location.href));
  const passwordId = useId();

  const finish = useCallback(
    (user: User) => {
      // The application that sent the person here is still waiting on its consent page.
      if (consent) leave(consent);
      else onUser(user);
    },
    [consent, leave, onUser],
  );

  useEffect(() => {
    login.setHandlers({
      available: setPasskeys,
      waiting: setWaiting,
      signedIn: finish,
      failed: (code) => {
        setError(PASSKEY_ERRORS.has(code) ? t(`errors.${code}`) : t("errors.passkey_login"));
      },
    });
  }, [login, finish, t]);

  useEffect(() => {
    if (!supported) return;
    void login.start();
    // A page brought back from the back/forward cache holds a stale challenge.
    const onShow = (event: PageTransitionEvent) => {
      if (event.persisted) void login.start();
    };
    window.addEventListener("pageshow", onShow);
    return () => {
      window.removeEventListener("pageshow", onShow);
      login.stop();
    };
  }, [login, supported]);

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setPending(true);
    setError(null);
    const form = new FormData(event.currentTarget);
    try {
      const result = await api.login(String(form.get("username")), String(form.get("password")));
      finish(result.user);
    } catch (cause) {
      const code = cause instanceof ApiError ? cause.code : "network";
      setError(t(`errors.${code}`, { defaultValue: t("errors.login") }));
    } finally {
      setPending(false);
    }
  }

  function signInWithPasskey() {
    setError(null);
    login.signIn();
  }

  const busy = pending || waiting;
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
            {consent ? <p className="auth-eyebrow auth-rise" style={step(0)}>{t("login.connectEyebrow")}</p> : null}
            <h1 className="auth-title auth-rise" style={step(1)}>{t("login.title")}</h1>
            {consent ? <p className="auth-lede auth-rise" style={step(2)}>{t("login.connectHint")}</p> : null}
          </div>

          <div className="auth-fields">
            <label className={`${fieldClass} auth-rise`} style={step(3)}>
              <span className="line-field-label">{t("login.username")}</span>
              <input
                name="username"
                autoComplete={supported ? "username webauthn" : "username"}
                autoCapitalize="none"
                spellCheck={false}
                autoFocus
                required
                disabled={busy}
                onChange={(event) => login.setUsername(event.target.value)}
              />
            </label>
            <div className={`${fieldClass} auth-rise`} style={step(4)}>
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
                autoComplete="current-password"
                autoCapitalize="none"
                spellCheck={false}
                required
                disabled={busy}
              />
            </div>
          </div>

          <div className="auth-foot">
            {error ? <p className="auth-error" role="alert">{error}</p> : null}
            <button className="auth-submit auth-rise" style={step(5)} type="submit" disabled={busy} aria-busy={pending}>
              <span>{pending ? t("login.pending") : t("login.submit")}</span>
              <ArrowRight aria-hidden="true" />
              {pending ? <span className="auth-sweep" aria-hidden="true" /> : null}
            </button>
            {supported && passkeys ? (
              <>
                <p className="auth-or" aria-hidden="true"><span>{t("login.or")}</span></p>
                <button className="auth-passkey" type="button" disabled={busy} aria-busy={waiting} onClick={signInWithPasskey}>
                  <KeyRound aria-hidden="true" />
                  <span>{waiting ? t("login.passkeyWaiting") : t("login.passkey")}</span>
                </button>
              </>
            ) : null}
          </div>
        </form>
      </section>
    </main>
  );
}

/** The left half: a stack of papers fed in, one read, its fields lifted out and the page stamped.
 *  What paperjam does, drawn rather than said. Decoration only. */
export function PaperStage() {
  return (
    <div className="auth-stage">
      <div className="paper-stack" aria-hidden="true">
        <div className="paper-sheet paper-sheet--back" />
        <div className="paper-sheet paper-sheet--mid" />
        <div className="paper-sheet paper-sheet--front">
          <div className="paper-fold" />
          <span className="paper-bar paper-bar--title" />
          <div className="paper-meta">
            <span className="paper-bar paper-bar--meta" />
            <span className="paper-bar paper-bar--meta paper-bar--short" />
          </div>
          <span className="paper-line" style={{ "--w": "92%" } as CSSProperties} />
          <span className="paper-line paper-line--hit" style={{ "--w": "68%" } as CSSProperties} />
          <span className="paper-line" style={{ "--w": "84%" } as CSSProperties} />
          <span className="paper-line paper-line--hit paper-line--late" style={{ "--w": "46%" } as CSSProperties} />
          <span className="paper-line" style={{ "--w": "74%" } as CSSProperties} />
          <span className="paper-line" style={{ "--w": "58%" } as CSSProperties} />
          <span className="paper-scan" />
          <span className="paper-stamp">
            <Check />
          </span>
        </div>
        <div className="paper-chips">
          {[0, 1, 2].map((index) => (
            <span className="paper-chip" key={index} style={step(index)}>
              <span className="paper-chip-dot" />
              <span className="paper-chip-key" />
              <span className="paper-chip-value" />
            </span>
          ))}
        </div>
      </div>
    </div>
  );
}
