import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { I18nextProvider } from "react-i18next";
import { afterEach, describe, expect, it } from "vitest";

import { createI18n } from "../../i18n";
import { ToastHost } from "./ToastHost";
import { clearToasts, toast } from "./toast";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root | null = null;
let container: HTMLDivElement | null = null;

afterEach(async () => {
  if (root) await act(async () => root?.unmount());
  container?.remove();
  root = null;
  container = null;
  clearToasts();
});

describe("ToastHost", () => {
  it("renders localized undo and dismiss controls", async () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    await act(async () => {
      root?.render(
        <I18nextProvider i18n={createI18n("en")}>
          <ToastHost />
        </I18nextProvider>
      );
    });

    await act(async () => {
      toast("Saved", { undo: () => {} });
    });

    expect(container.querySelector('[role="region"]')?.getAttribute("aria-label")).toBe("Notifications");
    expect(container.querySelector(".toast-message")?.textContent).toBe("Saved");
    expect(container.querySelector(".toast-undo")?.textContent).toBe("Undo");
    expect(container.querySelector(".toast-dismiss")?.getAttribute("aria-label")).toBe("Dismiss notification");
  });
});
