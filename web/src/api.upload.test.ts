import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ApiError, api } from "./api";

/** Records the request and lets the test fire the transport events by hand. */
class FakeXMLHttpRequest {
  static last: FakeXMLHttpRequest | undefined;
  method = "";
  url = "";
  body: FormData | undefined;
  withCredentials = false;
  status = 0;
  statusText = "";
  responseText = "";
  upload: { onprogress: ((event: { lengthComputable: boolean; loaded: number; total: number }) => void) | null } = { onprogress: null };
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;

  constructor() {
    FakeXMLHttpRequest.last = this;
  }

  open(method: string, url: string) {
    this.method = method;
    this.url = url;
  }

  send(body: FormData) {
    this.body = body;
  }
}

function syntheticFile(): File {
  return new File(["synthetic"], "statement.pdf", { type: "application/pdf" });
}

function current(): FakeXMLHttpRequest {
  const xhr = FakeXMLHttpRequest.last;
  if (!xhr) throw new Error("No upload request was sent");
  return xhr;
}

beforeEach(() => {
  FakeXMLHttpRequest.last = undefined;
  vi.stubGlobal("XMLHttpRequest", FakeXMLHttpRequest);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("api.upload", () => {
  it("appends the options before the file part, in order", async () => {
    const pending = api.upload(syntheticFile(), { localOnly: true, typeId: "type-bank", people: ["person-a", "person-b"], tags: ["tag-x"] });
    const xhr = current();
    expect(xhr.method).toBe("POST");
    expect(xhr.url).toBe("/api/v1/documents");
    expect(Array.from(xhr.body?.keys() ?? [])).toEqual(["local_only", "type_id", "people", "people", "tags", "file"]);
    expect(xhr.body?.getAll("people")).toEqual(["person-a", "person-b"]);
    expect(xhr.body?.get("local_only")).toBe("true");

    xhr.status = 201;
    xhr.responseText = JSON.stringify({ id: "doc-new", status: "queued", restored: false });
    xhr.onload?.();
    await expect(pending).resolves.toEqual({ id: "doc-new", status: "queued", restored: false });
  });

  it("omits type_id when no type is chosen", async () => {
    const pending = api.upload(syntheticFile(), { localOnly: false });
    const xhr = current();
    expect(Array.from(xhr.body?.keys() ?? [])).toEqual(["local_only", "file"]);
    xhr.status = 201;
    xhr.responseText = "{}";
    xhr.onload?.();
    await pending;
  });

  it("reports the fraction uploaded only when the length is known", async () => {
    const onProgress = vi.fn();
    const pending = api.upload(syntheticFile(), { localOnly: false }, onProgress);
    const xhr = current();

    xhr.upload.onprogress?.({ lengthComputable: false, loaded: 10, total: 0 });
    expect(onProgress).not.toHaveBeenCalled();

    xhr.upload.onprogress?.({ lengthComputable: true, loaded: 50, total: 100 });
    expect(onProgress).toHaveBeenCalledOnce();
    expect(onProgress).toHaveBeenCalledWith(0.5);

    xhr.status = 201;
    xhr.responseText = "{}";
    xhr.onload?.();
    await pending;
  });

  it("rejects a non-2xx response with the server's error code and document id", async () => {
    const pending = api.upload(syntheticFile(), { localOnly: false });
    const xhr = current();
    xhr.status = 409;
    xhr.statusText = "Conflict";
    xhr.responseText = JSON.stringify({ code: "duplicate", message: "x", document_id: "d1" });
    xhr.onload?.();

    const error = await pending.catch((cause: unknown) => cause);
    expect(error).toBeInstanceOf(ApiError);
    expect(error).toMatchObject({ status: 409, code: "duplicate", documentId: "d1", message: "x" });
  });

  it("falls back to an unknown code when the error body is not JSON", async () => {
    const pending = api.upload(syntheticFile(), { localOnly: false });
    const xhr = current();
    xhr.status = 500;
    xhr.statusText = "Internal Server Error";
    xhr.responseText = "<html>";
    xhr.onload?.();

    await expect(pending).rejects.toMatchObject({ status: 500, code: "unknown", message: "Internal Server Error" });
  });

  it("rejects with a network TypeError when the request never completes", async () => {
    const pending = api.upload(syntheticFile(), { localOnly: false });
    current().onerror?.();
    await expect(pending).rejects.toThrow(new TypeError("network"));
  });
});
