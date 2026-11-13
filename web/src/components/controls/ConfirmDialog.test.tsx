import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ConfirmDialog } from "./ConfirmDialog";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let root: Root | null = null;
let container: HTMLDivElement | null = null;

afterEach(async () => {
  if (root) await act(async () => root?.unmount());
  container?.remove();
  root = null;
  container = null;
});

async function mount(onCancel: () => void = () => undefined) {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await act(async () => {
    root?.render(
      <>
        <button type="button" id="behind">Behind</button>
        <ConfirmDialog
          open
          title="Discard?"
          description="Edits are not saved."
          confirmLabel="Discard"
          cancelLabel="Keep"
          onConfirm={() => undefined}
          onCancel={onCancel}
        />
      </>,
    );
  });
}

async function press(key: string, init: KeyboardEventInit = {}) {
  const event = new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, ...init });
  await act(async () => {
    (document.activeElement ?? document.body).dispatchEvent(event);
  });
  return event;
}

const buttons = () => Array.from(document.querySelectorAll<HTMLButtonElement>('[role="alertdialog"] button'));

describe("ConfirmDialog", () => {
  it("starts on the cancel button", async () => {
    await mount();
    expect(document.activeElement).toBe(buttons()[0]);
  });

  it("keeps Tab and Shift+Tab inside the dialog", async () => {
    await mount();
    const [cancel, confirm] = buttons();

    // From the last button Tab wraps to the first; from the first Shift+Tab wraps to the last.
    confirm.focus();
    const forward = await press("Tab");
    expect(forward.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(cancel);

    const backward = await press("Tab", { shiftKey: true });
    expect(backward.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(confirm);

    // Between the ends the browser moves focus itself; the trap must not interfere.
    cancel.focus();
    expect((await press("Tab")).defaultPrevented).toBe(false);
  });

  it("pulls focus back in when it is somewhere behind the dialog", async () => {
    await mount();
    (document.getElementById("behind") as HTMLButtonElement).focus();
    const event = await press("Tab");
    expect(event.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(buttons()[0]);
  });

  it("cancels on Escape", async () => {
    const onCancel = vi.fn();
    await mount(onCancel);
    await press("Escape");
    expect(onCancel).toHaveBeenCalledTimes(1);
  });
});
