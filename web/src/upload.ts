import { ApiError, api, type DocumentStatus, type UploadOptions } from "./api";

export interface UploadUpdate {
  phase: "uploading" | DocumentStatus | "duplicate";
  documentId?: string;
  // Carried by status polls only: the upload response has no title, and a title may still be null.
  title?: string | null;
  restored?: boolean;
  code?: string;
  progress?: number;
}

interface UploadDependencies {
  upload: typeof api.upload;
  document: (id: string) => Promise<{ status: DocumentStatus; title?: string | null }>;
  wait: (milliseconds: number) => Promise<void>;
}

const defaultDependencies: UploadDependencies = {
  upload: api.upload,
  document: api.document,
  wait: (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds)),
};

export async function runUpload(
  file: File,
  options: UploadOptions,
  onUpdate: (update: UploadUpdate) => void,
  dependencies: UploadDependencies = defaultDependencies,
): Promise<void> {
  let documentId: string | undefined;
  onUpdate({ phase: "uploading" });
  try {
    const uploaded = await dependencies.upload(file, options, (progress) => onUpdate({ phase: "uploading", progress }));
    documentId = uploaded.id;
    onUpdate({ phase: uploaded.status, documentId, restored: uploaded.restored });
    let status = uploaded.status;
    while (status === "queued" || status === "processing") {
      await dependencies.wait(1200);
      const document = await dependencies.document(documentId);
      status = document.status;
      onUpdate({ phase: status, documentId, ...(document.title !== undefined ? { title: document.title } : {}) });
    }
  } catch (cause) {
    const code = cause instanceof ApiError ? cause.code : "network";
    if (code === "duplicate") {
      onUpdate({ phase: "duplicate", documentId: cause instanceof ApiError ? cause.documentId : undefined, code });
      return;
    }
    onUpdate(documentId ? { phase: "failed", documentId, code } : { phase: "failed", code });
  }
}
