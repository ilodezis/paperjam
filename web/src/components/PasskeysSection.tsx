import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Fingerprint, Plus, Trash2 } from "lucide-react";
import { useRef, useState, type FormEvent, type ReactNode } from "react";
import { useTranslation } from "react-i18next";

import { ApiError, api, type PasskeyView } from "../api";
import { creationOptions, credentialJSON, failureName, passkeysSupported, signalAccepted } from "../passkeys";
import { useConfirm } from "./controls/ConfirmDialog";
import { toast } from "./controls/toast";

function formatDate(value: string, language: string): string {
  return new Intl.DateTimeFormat(language, { dateStyle: "medium" }).format(new Date(value));
}

/**
 * The passkeys the signed-in user signs in with: the list, adding one, removing one.
 *
 * Adding is two taps on purpose. The first fetches the options while the user names the key;
 * the second opens the browser's sheet straight from the tap. Safari refuses a sheet that only
 * arrives after a network round trip, so the fetch cannot sit between the tap and the sheet.
 */
export function PasskeysSection() {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();
  const { confirm, dialogElement } = useConfirm();
  const language = i18n.resolvedLanguage ?? "en";
  const { data } = useQuery({ queryKey: ["passkeys"], queryFn: api.passkeys, staleTime: 5 * 60 * 1000 });
  const passkeys = data?.passkeys ?? [];

  // null: not adding. Otherwise the ceremony for the sheet, once it has arrived.
  const [adding, setAdding] = useState<{ ceremony: string; options: PublicKeyCredentialCreationOptions } | "loading" | null>(null);
  const [creating, setCreating] = useState(false);
  const [label, setLabel] = useState("");
  const [busy, setBusy] = useState(false);
  // Bumped by Cancel, so options that arrive afterwards do not reopen the form.
  const attempt = useRef(0);

  const refresh = () => queryClient.invalidateQueries({ queryKey: ["passkeys"] });

  function failure(cause: unknown, fallback: string) {
    const code = cause instanceof ApiError ? cause.code : "network";
    toast(t(`errors.${code}`, { defaultValue: fallback }), { icon: "error" });
  }

  async function fetchOptions() {
    const mine = ++attempt.current;
    setAdding("loading");
    try {
      const challenge = await api.passkeyChallenge();
      if (mine === attempt.current) setAdding({ ceremony: challenge.ceremony, options: creationOptions(challenge.options) });
    } catch (cause) {
      if (mine !== attempt.current) return;
      setAdding(null);
      failure(cause, t("errors.passkey_add"));
    }
  }

  function cancel() {
    attempt.current += 1;
    setAdding(null);
    setLabel("");
  }

  function create(event: FormEvent) {
    event.preventDefault();
    if (adding === null || adding === "loading" || creating) return;
    const { ceremony, options } = adding;
    const name = label.trim() || t("settings.passkeyDefaultLabel");
    setCreating(true);
    // No await before this call: it has to run inside the tap.
    navigator.credentials
      .create({ publicKey: options })
      .then(async (credential) => {
        if (!credential) throw new Error("no credential");
        await api.addPasskey(ceremony, name, credentialJSON(credential as PublicKeyCredential));
        toast(t("settings.passkeyAddedToast"), { icon: "check" });
        attempt.current += 1;
        setAdding(null);
        setLabel("");
        await refresh();
      })
      .catch((cause: unknown) => {
        // The challenge is spent either way: arm a fresh one for the next tap.
        void fetchOptions();
        const reason = failureName(cause);
        // NotAllowedError: the sheet was closed. Nothing to report.
        if (reason === "NotAllowedError") return;
        if (reason === "InvalidStateError") {
          toast(t("errors.passkey_already_registered"), { icon: "alert" });
          return;
        }
        failure(cause, t("errors.passkey_add"));
      })
      .finally(() => setCreating(false));
  }

  async function remove(passkey: PasskeyView) {
    const ok = await confirm({
      title: t("settings.removePasskeyFor", { name: passkey.label }),
      description: t("settings.removePasskeyConfirm", { name: passkey.label }),
      confirmLabel: t("settings.removePasskey"),
      cancelLabel: t("common.cancel"),
      destructive: true,
    });
    if (!ok) return;
    setBusy(true);
    try {
      await api.removePasskey(passkey.id);
      toast(t("settings.passkeyRemovedToast"), { icon: "check" });
      if (data?.rp_id) {
        signalAccepted(data.rp_id, data.user_handle, passkeys.filter((p) => p.id !== passkey.id).map((p) => p.credential_id));
      }
      await refresh();
    } catch (cause) {
      failure(cause, t("errors.settings"));
    } finally {
      setBusy(false);
    }
  }

  let adder: ReactNode;
  if (data && !data.available) {
    adder = <p className="settings-note">{t("settings.passkeysUnavailable")}</p>;
  } else if (!passkeysSupported()) {
    adder = <p className="settings-note">{t("settings.passkeysUnsupported")}</p>;
  } else if (adding === null) {
    adder = (
      <div className="passkey-add">
        <button type="button" className="button button--secondary" disabled={!data} onClick={() => void fetchOptions()}>
          <Plus />{t("settings.addPasskey")}
        </button>
      </div>
    );
  } else {
    const loading = adding === "loading";
    adder = (
      <form className="token-create" onSubmit={create}>
        <label className="sr-only" htmlFor="passkey-label">{t("settings.passkeyLabel")}</label>
        <input
          id="passkey-label"
          maxLength={64}
          autoComplete="off"
          placeholder={t("settings.passkeyPlaceholder")}
          value={label}
          onChange={(event) => setLabel(event.target.value)}
          aria-label={t("settings.passkeyLabel")}
        />
        <button type="submit" className="button button--primary" disabled={loading || creating} aria-busy={loading || creating}>
          <Fingerprint />{t("settings.createPasskey")}
        </button>
        <button type="button" className="button button--secondary" onClick={cancel}>{t("common.cancel")}</button>
      </form>
    );
  }

  return (
    <section className="settings-section" aria-labelledby="settings-passkeys">
      <div className="settings-section-heading"><div><h3 id="settings-passkeys">{t("settings.passkeys")}</h3><p>{t("settings.passkeysDescription")}</p></div></div>
      {adder}
      {passkeys.length === 0 ? (
        data?.available ? <p className="settings-note">{t("settings.passkeysEmpty")}</p> : null
      ) : (
        <ul className="settings-records token-records">
          {passkeys.map((passkey) => (
            <li key={passkey.id}>
              <div>
                <strong>{passkey.label}</strong>
                <span>{t("settings.created", { date: formatDate(passkey.created_at, language) })}</span>
                <span>{passkey.last_used_at ? t("settings.lastSignIn", { date: formatDate(passkey.last_used_at, language) }) : t("settings.notUsedYet")}</span>
              </div>
              <button type="button" className="quiet-button" disabled={busy} onClick={() => void remove(passkey)} aria-label={t("settings.removePasskeyFor", { name: passkey.label })}>
                <Trash2 />{t("settings.removePasskey")}
              </button>
            </li>
          ))}
        </ul>
      )}
      {dialogElement}
    </section>
  );
}
