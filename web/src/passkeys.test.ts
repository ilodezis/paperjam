import { afterEach, describe, expect, it, vi } from "vitest";

import { creationOptions, credentialJSON, fromBase64url, requestOptions, toBase64url } from "./passkeys";

const bytes = (buffer: BufferSource | undefined): number[] => [...new Uint8Array(buffer as ArrayBuffer)];
const buffer = (...values: number[]): ArrayBuffer => Uint8Array.from(values).buffer;

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("base64url", () => {
  it("round-trips every padding length", () => {
    for (const length of [0, 1, 2, 3, 4, 31, 32, 33]) {
      const data = Uint8Array.from({ length }, (_, i) => (i * 37 + 251) % 256);
      expect(bytes(fromBase64url(toBase64url(data.buffer)))).toEqual([...data]);
    }
  });

  it("uses the URL alphabet and no padding, as the server writes it", () => {
    expect(toBase64url(buffer(0xfb, 0xff, 0xfe))).toBe("-__-");
    expect(bytes(fromBase64url("-__-"))).toEqual([0xfb, 0xff, 0xfe]);
  });
});

describe("creationOptions", () => {
  const json = {
    rp: { id: "paperjam.example.org", name: "paperjam" },
    user: { id: "AQID", name: "alice", displayName: "Alice" },
    challenge: "BAUG",
    pubKeyCredParams: [{ type: "public-key", alg: -7 }],
    excludeCredentials: [{ id: "BwgJ", type: "public-key", transports: ["internal"] }],
    authenticatorSelection: { residentKey: "preferred", userVerification: "required" },
    hints: null,
  };

  it("turns the byte fields into buffers and drops nulls without the native parser", () => {
    const options = creationOptions(json);
    expect(bytes(options.challenge)).toEqual([4, 5, 6]);
    expect(bytes(options.user.id)).toEqual([1, 2, 3]);
    expect(options.user.name).toBe("alice");
    expect(bytes(options.excludeCredentials?.[0]?.id)).toEqual([7, 8, 9]);
    expect(options.excludeCredentials?.[0]?.transports).toEqual(["internal"]);
    expect(options.authenticatorSelection?.userVerification).toBe("required");
    expect("hints" in options).toBe(false);
  });

  it("copes with no keys to exclude", () => {
    const rest: Record<string, unknown> = { ...json };
    delete rest.excludeCredentials;
    expect(creationOptions(rest).excludeCredentials).toEqual([]);
  });

  it("hands the JSON to the browser's parser when there is one", () => {
    const parsed = { challenge: buffer(9) } as unknown as PublicKeyCredentialCreationOptions;
    const parseCreationOptionsFromJSON = vi.fn(() => parsed);
    vi.stubGlobal("PublicKeyCredential", { parseCreationOptionsFromJSON });
    expect(creationOptions(json)).toBe(parsed);
    expect(parseCreationOptionsFromJSON).toHaveBeenCalledWith(expect.not.objectContaining({ hints: null }));
  });
});

describe("requestOptions", () => {
  it("decodes the challenge and the allowed credentials", () => {
    const options = requestOptions({
      challenge: "BAUG",
      rpId: "paperjam.example.org",
      allowCredentials: [{ id: "BwgJ", type: "public-key" }],
      userVerification: "required",
    });
    expect(bytes(options.challenge)).toEqual([4, 5, 6]);
    expect(options.rpId).toBe("paperjam.example.org");
    expect(bytes(options.allowCredentials?.[0]?.id)).toEqual([7, 8, 9]);
  });

  it("leaves a discoverable request without an allow-list empty", () => {
    expect(requestOptions({ challenge: "BAUG" }).allowCredentials).toEqual([]);
  });

  it("prefers the browser's parser", () => {
    const parsed = { challenge: buffer(1) } as unknown as PublicKeyCredentialRequestOptions;
    vi.stubGlobal("PublicKeyCredential", { parseRequestOptionsFromJSON: () => parsed });
    expect(requestOptions({ challenge: "BAUG" })).toBe(parsed);
  });
});

describe("credentialJSON", () => {
  const common = {
    id: "AQID",
    rawId: buffer(1, 2, 3),
    type: "public-key",
    authenticatorAttachment: "platform",
    getClientExtensionResults: () => ({ credProps: { rk: true } }),
  };

  it("spells a new credential the way the server parses it", () => {
    const credential = {
      ...common,
      response: { clientDataJSON: buffer(4), attestationObject: buffer(5), getTransports: () => ["internal", "hybrid"] },
    } as unknown as PublicKeyCredential;
    expect(credentialJSON(credential)).toEqual({
      id: "AQID",
      rawId: "AQID",
      type: "public-key",
      authenticatorAttachment: "platform",
      clientExtensionResults: { credProps: { rk: true } },
      response: { clientDataJSON: "BA", attestationObject: "BQ", transports: ["internal", "hybrid"] },
    });
  });

  it("spells an assertion with its user handle", () => {
    const credential = {
      ...common,
      response: { clientDataJSON: buffer(4), authenticatorData: buffer(5), signature: buffer(6), userHandle: buffer(7) },
    } as unknown as PublicKeyCredential;
    expect(credentialJSON(credential)).toMatchObject({
      rawId: "AQID",
      response: { clientDataJSON: "BA", authenticatorData: "BQ", signature: "Bg", userHandle: "Bw" },
    });
  });

  it("uses the browser's own toJSON when present", () => {
    const credential = { ...common, response: {}, toJSON: () => ({ id: "native" }) } as unknown as PublicKeyCredential;
    expect(credentialJSON(credential)).toEqual({ id: "native" });
  });
});
