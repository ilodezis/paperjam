import { ApiError, api, type User } from "./api";
import { conditionalMediationAvailable, credentialJSON, failureName, requestOptions, signalUnknown } from "./passkeys";

export interface PasskeyLoginHandlers {
  /** Whether the server offers passkeys at all (`PAPERJAM_PUBLIC_URL` is set). */
  available(value: boolean): void;
  /** True while the browser's sheet is open or the answer is being checked. */
  waiting(value: boolean): void;
  signedIn(user: User): void;
  /** A machine code: an API error code, `network`, or `passkey_failed`. */
  failed(code: string): void;
}

interface Armed {
  ceremony: string;
  options: PublicKeyCredentialRequestOptions;
  username: string;
}

/** The server forgets a challenge after five minutes; a fresh one is fetched before that. */
export const REARM_MS = 4 * 60 * 1000;
const USERNAME_DEBOUNCE_MS = 500;

const noop: PasskeyLoginHandlers = { available() {}, waiting() {}, signedIn() {}, failed() {} };

/**
 * The login page's passkey sign-in. A challenge is always fetched ahead of
 * time, so a tap opens the browser's sheet straight away: Safari refuses a sheet
 * that only arrives after a network round trip. Where the browser supports it,
 * the same challenge also offers the passkey in the username field's autofill.
 * With a username typed in, the next challenge allows only that account's
 * passkeys; without one, any paperjam passkey on the device.
 */
export class PasskeyLogin {
  private handlers = noop;
  private armed: Armed | null = null;
  private generation = 0;
  private autofill: AbortController | null = null;
  private refresh: ReturnType<typeof setTimeout> | undefined;
  private debounce: ReturnType<typeof setTimeout> | undefined;
  private conditional = false;
  private busy = false;
  private stopped = true;
  private username = "";

  setHandlers(handlers: PasskeyLoginHandlers): void {
    this.handlers = handlers;
  }

  async start(): Promise<void> {
    this.stopped = false;
    this.conditional = await conditionalMediationAvailable();
    if (!this.stopped) await this.arm();
  }

  stop(): void {
    this.stopped = true;
    this.generation += 1;
    this.stopListening();
    clearTimeout(this.refresh);
    clearTimeout(this.debounce);
  }

  /** The username field changed: arm a challenge for that account once typing pauses. */
  setUsername(value: string): void {
    this.username = value.trim();
    clearTimeout(this.debounce);
    if (this.stopped || this.busy || this.armed?.username === this.username) return;
    this.debounce = setTimeout(() => void this.arm(), USERNAME_DEBOUNCE_MS);
  }

  /** The "Sign in with a passkey" tap. */
  signIn(): void {
    if (this.busy) return;
    const armed = this.armed;
    if (armed && armed.username === this.username) {
      this.prompt(armed);
      return;
    }
    // Nothing ready (or it names another account): fetch, then ask.
    void this.arm().then((next) => {
      if (next) this.prompt(next);
      else if (!this.stopped) this.handlers.failed("passkey_failed");
    });
  }

  private stopListening(): void {
    this.autofill?.abort();
    this.autofill = null;
  }

  private async arm(): Promise<Armed | null> {
    if (this.stopped) return null;
    const mine = ++this.generation;
    const username = this.username;
    this.stopListening();
    clearTimeout(this.refresh);
    this.armed = null;
    try {
      const challenge = await api.passkeyLoginChallenge(username || undefined);
      if (mine !== this.generation) return null;
      const armed = { ceremony: challenge.ceremony, options: requestOptions(challenge.options), username };
      this.armed = armed;
      this.handlers.available(true);
      this.refresh = setTimeout(() => void this.arm(), REARM_MS);
      this.listen(armed);
      return armed;
    } catch (cause) {
      if (mine !== this.generation) return null;
      if (cause instanceof ApiError && cause.code === "passkeys_unavailable") this.handlers.available(false);
      return null;
    }
  }

  /** Offers the armed challenge in the username field's autofill, where the browser can. */
  private listen(armed: Armed): void {
    if (!this.conditional || this.busy) return;
    const controller = new AbortController();
    this.autofill = controller;
    navigator.credentials
      .get({ publicKey: armed.options, mediation: "conditional", signal: controller.signal } as CredentialRequestOptions)
      .then((credential) => {
        if (credential && this.autofill === controller) void this.finish(armed, credential as PublicKeyCredential);
      })
      .catch(() => {
        // Aborted for a fresh challenge, or dismissed: nothing to report.
      });
  }

  private prompt(armed: Armed): void {
    this.stopListening();
    this.setBusy(true);
    // No await before this call: it has to run inside the tap.
    navigator.credentials
      .get({ publicKey: armed.options })
      .then(
        (credential) => {
          if (credential) return this.finish(armed, credential as PublicKeyCredential);
          this.setBusy(false);
          void this.arm();
        },
        (error: unknown) => {
          this.setBusy(false);
          // NotAllowedError: the sheet was closed. Nothing to apologise for.
          const name = failureName(error);
          if (name !== "NotAllowedError" && name !== "AbortError") this.handlers.failed("passkey_failed");
          void this.arm();
        },
      );
  }

  private async finish(armed: Armed, credential: PublicKeyCredential): Promise<void> {
    this.stopListening();
    clearTimeout(this.refresh);
    this.armed = null;
    this.setBusy(true);
    try {
      const result = await api.passkeyLogin(armed.ceremony, credentialJSON(credential));
      this.stop();
      this.handlers.signedIn(result.user);
    } catch (cause) {
      const code = cause instanceof ApiError ? cause.code : "network";
      // Removed here, still on the device: where the browser can, tell its keychain.
      if (code === "unknown_passkey" && armed.options.rpId) signalUnknown(armed.options.rpId, credential.id);
      this.handlers.failed(code);
      this.setBusy(false);
      void this.arm();
    }
  }

  private setBusy(value: boolean): void {
    this.busy = value;
    this.handlers.waiting(value);
  }
}
