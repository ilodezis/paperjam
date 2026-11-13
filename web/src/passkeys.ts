/**
 * The browser half of passkeys: the server's WebAuthn JSON options in, the
 * credential out as JSON the server parses (bytes as base64url).
 *
 * Uses `PublicKeyCredential.parseCreationOptionsFromJSON()` /
 * `parseRequestOptionsFromJSON()` and `credential.toJSON()` where the browser
 * has them, and a small base64url conversion where it does not (older Safari).
 */

type Json = Record<string, unknown>;

interface JsonDescriptor {
  id: string;
  type: PublicKeyCredentialType;
  transports?: AuthenticatorTransport[];
}

interface JsonCreationOptions extends Omit<PublicKeyCredentialCreationOptions, "challenge" | "user" | "excludeCredentials"> {
  challenge: string;
  user: { id: string; name: string; displayName: string };
  excludeCredentials?: JsonDescriptor[];
}

interface JsonRequestOptions extends Omit<PublicKeyCredentialRequestOptions, "challenge" | "allowCredentials"> {
  challenge: string;
  allowCredentials?: JsonDescriptor[];
}

interface ParseApi {
  parseCreationOptionsFromJSON?: (options: Json) => PublicKeyCredentialCreationOptions;
  parseRequestOptionsFromJSON?: (options: Json) => PublicKeyCredentialRequestOptions;
  isConditionalMediationAvailable?: () => Promise<boolean>;
  signalUnknownCredential?: (options: { rpId: string; credentialId: string }) => Promise<void>;
  signalAllAcceptedCredentials?: (options: { rpId: string; userId: string; allAcceptedCredentialIds: string[] }) => Promise<void>;
}

function credentialApi(): ParseApi | undefined {
  return typeof window === "undefined" ? undefined : (window.PublicKeyCredential as unknown as ParseApi | undefined);
}

export function fromBase64url(value: string): ArrayBuffer {
  const base64 = value.replace(/-/g, "+").replace(/_/g, "/");
  const binary = atob(base64 + "===".slice((base64.length + 3) % 4));
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
  return bytes.buffer;
}

export function toBase64url(buffer: ArrayBuffer): string {
  const bytes = new Uint8Array(buffer);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** Drops `null` members: WebAuthn dictionaries take absent, not null. */
function withoutNulls<T>(value: T): T {
  if (Array.isArray(value)) return value.map(withoutNulls) as T;
  if (value && typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value as Json).filter(([, v]) => v !== null).map(([k, v]) => [k, withoutNulls(v)]),
    ) as T;
  }
  return value;
}

export function passkeysSupported(): boolean {
  return typeof window !== "undefined" && Boolean(window.PublicKeyCredential) && typeof navigator.credentials?.get === "function";
}

/** Whether the browser can offer passkeys in the username field's autofill. */
export async function conditionalMediationAvailable(): Promise<boolean> {
  const check = credentialApi()?.isConditionalMediationAvailable;
  if (typeof check !== "function") return false;
  try {
    return await check.call(window.PublicKeyCredential);
  } catch {
    return false;
  }
}

/** `POST /passkeys/challenge` options → what `navigator.credentials.create()` takes. */
export function creationOptions(json: Json): PublicKeyCredentialCreationOptions {
  const clean = withoutNulls(json);
  const parse = credentialApi()?.parseCreationOptionsFromJSON;
  if (typeof parse === "function") return parse.call(window.PublicKeyCredential, clean);
  const options = clean as unknown as JsonCreationOptions;
  return {
    ...options,
    challenge: fromBase64url(options.challenge),
    user: { ...options.user, id: fromBase64url(options.user.id) },
    excludeCredentials: (options.excludeCredentials ?? []).map((c) => ({ ...c, id: fromBase64url(c.id) })),
  };
}

/** `POST /auth/passkey/challenge` options → what `navigator.credentials.get()` takes. */
export function requestOptions(json: Json): PublicKeyCredentialRequestOptions {
  const clean = withoutNulls(json);
  const parse = credentialApi()?.parseRequestOptionsFromJSON;
  if (typeof parse === "function") return parse.call(window.PublicKeyCredential, clean);
  const options = clean as unknown as JsonRequestOptions;
  return {
    ...options,
    challenge: fromBase64url(options.challenge),
    allowCredentials: (options.allowCredentials ?? []).map((c) => ({ ...c, id: fromBase64url(c.id) })),
  };
}

/** A new or signing credential, spelled the way the server parses it. */
export function credentialJSON(credential: PublicKeyCredential): Json {
  const native = (credential as unknown as { toJSON?: () => Json }).toJSON;
  if (typeof native === "function") return native.call(credential);
  const response = credential.response;
  const common = {
    id: credential.id,
    rawId: toBase64url(credential.rawId),
    type: credential.type,
    authenticatorAttachment: credential.authenticatorAttachment ?? undefined,
    clientExtensionResults: credential.getClientExtensionResults(),
  };
  if ("attestationObject" in response) {
    const attestation = response as AuthenticatorAttestationResponse;
    return {
      ...common,
      response: {
        clientDataJSON: toBase64url(attestation.clientDataJSON),
        attestationObject: toBase64url(attestation.attestationObject),
        transports: typeof attestation.getTransports === "function" ? attestation.getTransports() : [],
      },
    };
  }
  const assertion = response as AuthenticatorAssertionResponse;
  return {
    ...common,
    response: {
      clientDataJSON: toBase64url(assertion.clientDataJSON),
      authenticatorData: toBase64url(assertion.authenticatorData),
      signature: toBase64url(assertion.signature),
      userHandle: assertion.userHandle ? toBase64url(assertion.userHandle) : undefined,
    },
  };
}

/** The `name` of a DOMException-like rejection from the WebAuthn calls. */
export function failureName(error: unknown): string {
  return error && typeof error === "object" && "name" in error ? String((error as { name: unknown }).name) : "";
}

/** A passkey removed here but still on the device: ask its keychain to stop offering it. */
export function signalUnknown(rpId: string, credentialId: string): void {
  const signal = credentialApi()?.signalUnknownCredential;
  if (typeof signal !== "function") return;
  signal.call(window.PublicKeyCredential, { rpId, credentialId }).catch(() => {});
}

/** Tell the device's keychain which paperjam passkeys still work, so a removed one stops being
 *  offered at sign-in. Only where the browser has the Signal API; elsewhere a no-op. */
export function signalAccepted(rpId: string, userId: string, credentialIds: string[]): void {
  const signal = credentialApi()?.signalAllAcceptedCredentials;
  if (typeof signal !== "function") return;
  signal.call(window.PublicKeyCredential, { rpId, userId, allAcceptedCredentialIds: credentialIds }).catch(() => {});
}
