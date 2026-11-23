import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it } from "vitest";

import { SelectDropdown } from "./SelectDropdown";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let root: Root | null = null;
let container: HTMLDivElement | null = null;

afterEach(async () => {
  if (root) await act(async () => root?.unmount());
  container?.remove();
  root = null;
  container = null;
});

const options = [
  { value: "relevance", label: "Relevance" },
  { value: "date_desc", label: "Newest first" },
  { value: "date_asc", label: "Oldest first", disabled: true },
  { value: "name", label: "Name" },
];

function Harness({ initial = "relevance", placeholder, name }: { initial?: string; placeholder?: string; name?: string }) {
  const [value, setValue] = useState(initial);
  return (
    <form>
      <SelectDropdown value={value} onChange={setValue} options={options} aria-label="Sort" placeholder={placeholder} name={name} />
      <output>{value}</output>
    </form>
  );
}

async function mount(props: Parameters<typeof Harness>[0] = {}) {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await act(async () => {
    root?.render(<Harness {...props} />);
  });
  return container.querySelector<HTMLButtonElement>('[role="combobox"]')!;
}

async function key(target: Element, pressed: string) {
  await act(async () => {
    target.dispatchEvent(new KeyboardEvent("keydown", { key: pressed, bubbles: true, cancelable: true }));
  });
}

async function click(target: Element) {
  await act(async () => {
    target.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

const optionLabels = () => Array.from(container?.querySelectorAll('[role="option"]') ?? []).map((node) => node.textContent);
const value = () => container?.querySelector("output")?.textContent;

describe("SelectDropdown", () => {
  it("exposes one control with the accessible name and no native select behind it", async () => {
    const trigger = await mount();
    expect(container?.querySelectorAll("select")).toHaveLength(0);
    expect(container?.querySelectorAll('[aria-label="Sort"]')).toHaveLength(1);
    expect(trigger.getAttribute("aria-label")).toBe("Sort");
    expect(trigger.getAttribute("aria-expanded")).toBe("false");
    expect(trigger.textContent).toBe("Relevance");
  });

  it("lists only the real options, with no placeholder row, even when a placeholder is set", async () => {
    const trigger = await mount({ placeholder: "Select…" });
    await click(trigger);
    expect(optionLabels()).toEqual(["Relevance", "Newest first", "Oldest first", "Name"]);
    expect(container?.querySelector('[role="listbox"]')?.getAttribute("aria-label")).toBe("Sort");
    expect(trigger.getAttribute("aria-expanded")).toBe("true");
    expect(trigger.getAttribute("aria-controls")).toBe(container?.querySelector('[role="listbox"]')?.id);
  });

  it("shows the placeholder on the trigger only while nothing is chosen", async () => {
    const trigger = await mount({ initial: "", placeholder: "Pick one" });
    expect(trigger.textContent).toBe("Pick one");
    await click(trigger);
    expect(optionLabels()).not.toContain("Pick one");
  });

  it("moves with the arrow keys, skips disabled options, and selects with Enter", async () => {
    const trigger = await mount();
    await key(trigger, "ArrowDown");
    const active = () => container?.querySelector(`#${CSS.escape(trigger.getAttribute("aria-activedescendant") ?? "")}`)?.textContent;
    expect(active()).toBe("Relevance");
    await key(trigger, "ArrowDown");
    expect(active()).toBe("Newest first");
    await key(trigger, "ArrowDown");
    expect(active()).toBe("Name");
    await key(trigger, "ArrowDown");
    expect(active()).toBe("Relevance");
    await key(trigger, "End");
    expect(active()).toBe("Name");
    await key(trigger, "Home");
    await key(trigger, "ArrowUp");
    expect(active()).toBe("Name");
    await key(trigger, "ArrowUp");
    await key(trigger, "Enter");

    expect(value()).toBe("date_desc");
    expect(container?.querySelector('[role="listbox"]')).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  it("closes on Escape without changing the value, and on Tab", async () => {
    const trigger = await mount();
    await click(trigger);
    await key(trigger, "ArrowDown");
    await key(trigger, "Escape");
    expect(container?.querySelector('[role="listbox"]')).toBeNull();
    expect(value()).toBe("relevance");

    await click(trigger);
    await key(trigger, "Tab");
    expect(container?.querySelector('[role="listbox"]')).toBeNull();
  });

  it("selects with a click and ignores clicks on disabled options", async () => {
    const trigger = await mount();
    await click(trigger);
    const items = Array.from(container?.querySelectorAll('[role="option"]') ?? []);
    expect(items[0].getAttribute("aria-selected")).toBe("true");
    expect(items[2].getAttribute("aria-disabled")).toBe("true");
    await click(items[2]);
    expect(value()).toBe("relevance");
    await click(items[3]);
    expect(value()).toBe("name");
    expect(trigger.textContent).toBe("Name");
  });

  it("closes when a pointer lands outside", async () => {
    const trigger = await mount();
    await click(trigger);
    await act(async () => {
      document.body.dispatchEvent(new Event("pointerdown", { bubbles: true }));
    });
    expect(container?.querySelector('[role="listbox"]')).toBeNull();
  });

  it("serializes a named select through a hidden input", async () => {
    await mount({ name: "sort" });
    const input = container?.querySelector<HTMLInputElement>('input[name="sort"]');
    expect(input?.type).toBe("hidden");
    expect(input?.value).toBe("relevance");
  });

  it("stays closed after selecting an option even when wrapped in a label", async () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    await act(async () => {
      root?.render(
        <label>
          <span>Sort</span>
          <Harness />
        </label>,
      );
    });
    const trigger = container.querySelector<HTMLButtonElement>('[role="combobox"]')!;
    await click(trigger);
    const items = Array.from(container.querySelectorAll('[role="option"]'));
    await click(items[3]);
    expect(value()).toBe("name");
    expect(container.querySelector('[role="listbox"]')).toBeNull();
  });

  it("flips upward and caps max-height when opened near the bottom bar on a phone", async () => {
    const bnav = document.createElement("nav");
    bnav.className = "bnav";
    bnav.getBoundingClientRect = () => ({ top: 760, bottom: 820, left: 0, right: 390, width: 390, height: 60, x: 0, y: 760, toJSON: () => ({}) }) as DOMRect;
    document.body.append(bnav);
    const prevHeight = window.innerHeight;
    Object.defineProperty(window, "innerHeight", { configurable: true, value: 820 });
    try {
      const trigger = await mount();
      trigger.getBoundingClientRect = () => ({ top: 690, bottom: 734, left: 16, right: 220, width: 204, height: 44, x: 16, y: 690, toJSON: () => ({}) }) as DOMRect;
      await click(trigger);
      const menu = container?.querySelector<HTMLElement>('[role="listbox"]');
      expect(menu?.classList.contains("select-dropdown-menu--up")).toBe(true);
      expect(menu?.style.maxHeight).toBe("260px");
    } finally {
      bnav.remove();
      Object.defineProperty(window, "innerHeight", { configurable: true, value: prevHeight });
    }
  });

  it("caps max-height to remaining viewport room when space is tight above and below", async () => {
    const prevHeight = window.innerHeight;
    Object.defineProperty(window, "innerHeight", { configurable: true, value: 300 });
    try {
      const trigger = await mount();
      trigger.getBoundingClientRect = () => ({ top: 100, bottom: 140, left: 16, right: 220, width: 204, height: 40, x: 16, y: 100, toJSON: () => ({}) }) as DOMRect;
      await click(trigger);
      const menu = container?.querySelector<HTMLElement>('[role="listbox"]');
      expect(menu?.classList.contains("select-dropdown-menu--up")).toBe(false);
      expect(menu?.style.maxHeight).toBe("148px");
    } finally {
      Object.defineProperty(window, "innerHeight", { configurable: true, value: prevHeight });
    }
  });
});
