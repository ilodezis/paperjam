import { describe, expect, it, vi } from "vitest";

import { ApiError, type Uploaded } from "./api";
import { runUpload, type UploadUpdate } from "./upload";

function syntheticFile(name = "statement.pdf"): File {
  return new File(["synthetic"], name, { type: "application/pdf" });
}

function uploaded(status: Uploaded["status"] = "queued"): Uploaded {
  return { id: "doc-new", status, restored: false };
}

describe("upload processing", () => {
  it("moves from upload through processing to ready", async () => {
    const updates: UploadUpdate[] = [];
    const upload = vi.fn().mockResolvedValue(uploaded());
    const document = vi.fn()
      .mockResolvedValueOnce({ status: "processing" })
      .mockResolvedValueOnce({ status: "ready" });
    const file = syntheticFile();

    await runUpload(file, { localOnly: true }, (update) => updates.push(update), {
      upload,
      document,
      wait: vi.fn().mockResolvedValue(undefined),
    });

    expect(upload).toHaveBeenCalledWith(file, { localOnly: true }, expect.any(Function));
    expect(document).toHaveBeenCalledTimes(2);
    expect(updates).toEqual([
      { phase: "uploading" },
      { phase: "queued", documentId: "doc-new", restored: false },
      { phase: "processing", documentId: "doc-new" },
      { phase: "ready", documentId: "doc-new" },
    ]);
  });

  it("reports upload progress from the transport callback", async () => {
    const updates: UploadUpdate[] = [];
    const upload = vi.fn(async (_file: File, _options: unknown, onProgress?: (fraction: number) => void) => {
      onProgress?.(0.5);
      return uploaded("ready");
    });

    await runUpload(syntheticFile(), { localOnly: false, typeId: "type-bank" }, (update) => updates.push(update), {
      upload,
      document: vi.fn(),
      wait: vi.fn(),
    });

    expect(upload).toHaveBeenCalledWith(expect.any(File), { localOnly: false, typeId: "type-bank" }, expect.any(Function));
    expect(updates[1]).toEqual({ phase: "uploading", progress: 0.5 });
  });

  it("stops at a failed processing status", async () => {
    const updates: UploadUpdate[] = [];
    const document = vi.fn().mockResolvedValue({ status: "failed" });

    await runUpload(syntheticFile(), { localOnly: false }, (update) => updates.push(update), {
      upload: vi.fn().mockResolvedValue(uploaded()),
      document,
      wait: vi.fn().mockResolvedValue(undefined),
    });

    expect(document).toHaveBeenCalledOnce();
    expect(updates.at(-1)).toEqual({ phase: "failed", documentId: "doc-new" });
  });

  it("reports a duplicate with the existing document id", async () => {
    const updates: UploadUpdate[] = [];
    const document = vi.fn();

    await runUpload(syntheticFile(), { localOnly: false }, (update) => updates.push(update), {
      upload: vi.fn().mockRejectedValue(new ApiError(409, "duplicate", "Already archived", undefined, "doc-existing")),
      document,
      wait: vi.fn(),
    });

    expect(document).not.toHaveBeenCalled();
    expect(updates).toEqual([
      { phase: "uploading" },
      { phase: "duplicate", documentId: "doc-existing", code: "duplicate" },
    ]);
  });

  it("surfaces upload and polling failures with stable error codes", async () => {
    const uploadUpdates: UploadUpdate[] = [];
    await runUpload(syntheticFile(), { localOnly: false }, (update) => uploadUpdates.push(update), {
      upload: vi.fn().mockRejectedValue(new ApiError(422, "unsupported_type", "Unsupported")),
      document: vi.fn(),
      wait: vi.fn(),
    });
    expect(uploadUpdates.at(-1)).toEqual({ phase: "failed", code: "unsupported_type" });

    const pollingUpdates: UploadUpdate[] = [];
    await runUpload(syntheticFile(), { localOnly: false }, (update) => pollingUpdates.push(update), {
      upload: vi.fn().mockResolvedValue(uploaded()),
      document: vi.fn().mockRejectedValue(new ApiError(503, "unknown", "Unavailable")),
      wait: vi.fn().mockResolvedValue(undefined),
    });
    expect(pollingUpdates.at(-1)).toEqual({ phase: "failed", documentId: "doc-new", code: "unknown" });
  });
});
