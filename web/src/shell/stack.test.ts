import { describe, expect, it } from "vitest";

import type { ScreenId } from "./nav";
import { backTarget, initialStack, planGo, planPop, topOf, withSearch, type Dest } from "./stack";

const at = (id: ScreenId, path: string, search = ""): Dest => ({ id, path, search });
const archive = at("archive", "/");
const doc = (n: string) => at("document", `/documents/${n}`);
const upload = at("upload", "/upload");
const settings = at("settings", "/settings");
const trash = at("trash", "/trash");

const paths = (s: { entries: { path: string }[] }) => s.entries.map((e) => e.path);

describe("screen stack", () => {
  it("starts with one screen and nowhere to go back to", () => {
    const s = initialStack(archive);
    expect(paths(s)).toEqual(["/"]);
    expect(backTarget(s)).toBeNull();
  });

  it("does nothing when asked for the screen already on top", () => {
    const plan = planGo(initialStack(archive), archive, { mode: "push", desktop: false });
    expect(plan.motion).toBe("none");
    expect(plan.from).toBeNull();
  });

  it("push slides a new screen over the current one and remembers where back goes", () => {
    const plan = planGo(initialStack(archive), doc("a"), { mode: "push", desktop: false });
    expect(plan.motion).toBe("push");
    expect(paths(plan.state)).toEqual(["/", "/documents/a"]);
    expect(plan.drop).toEqual([]);
    expect(backTarget(plan.state)).toBe("archive");
    expect(topOf(plan.state).id).toBe("document");
  });

  it("two documents are two screens: the path tells them apart, not the screen id", () => {
    const first = planGo(initialStack(archive), doc("a"), { mode: "push", desktop: false }).state;
    const plan = planGo(first, doc("b"), { mode: "push", desktop: false });
    expect(plan.motion).toBe("push");
    expect(paths(plan.state)).toEqual(["/", "/documents/a", "/documents/b"]);
  });

  it("a tab starts the stack over with a cross-fade and drops what was there", () => {
    const deep = planGo(initialStack(archive), doc("a"), { mode: "push", desktop: false }).state;
    const plan = planGo(deep, settings, { mode: "tab", desktop: false });
    expect(plan.motion).toBe("fade");
    expect(paths(plan.state)).toEqual(["/settings"]);
    expect(plan.drop.map((e) => e.path)).toEqual(["/", "/documents/a"]);
  });

  it("a sibling replaces the top screen and keeps what is below it", () => {
    const deep = planGo(initialStack(settings), trash, { mode: "push", desktop: false }).state;
    const plan = planGo(deep, upload, { mode: "replace", desktop: false });
    expect(plan.motion).toBe("fade");
    expect(paths(plan.state)).toEqual(["/settings", "/upload"]);
    expect(plan.drop.map((e) => e.path)).toEqual(["/trash"]);
    expect(backTarget(plan.state)).toBe("settings");
  });

  it("going to a screen already under the top pops back to it instead of stacking a copy", () => {
    let s = initialStack(archive);
    s = planGo(s, doc("a"), { mode: "push", desktop: false }).state;
    s = planGo(s, doc("b"), { mode: "push", desktop: false }).state;
    const plan = planGo(s, archive, { mode: "push", desktop: false });
    expect(plan.motion).toBe("pop");
    expect(paths(plan.state)).toEqual(["/"]);
    expect(plan.drop.map((e) => e.path)).toEqual(["/documents/a", "/documents/b"]);
    expect(plan.to?.id).toBe("archive");
  });

  it("the desktop shows one screen: every navigation cross-fades and keeps nothing", () => {
    const a = planGo(initialStack(archive), doc("a"), { mode: "push", desktop: true });
    expect(a.motion).toBe("fade");
    expect(paths(a.state)).toEqual(["/documents/a"]);
    const b = planGo(a.state, upload, { mode: "replace", desktop: true });
    expect(paths(b.state)).toEqual(["/upload"]);
  });

  it("a screen opened again is a new entry with a new key, never the old one revived", () => {
    let s = initialStack(archive);
    const first = topOf(s).key;
    s = planGo(s, settings, { mode: "tab", desktop: false }).state;
    s = planGo(s, archive, { mode: "tab", desktop: false }).state;
    expect(topOf(s).id).toBe("archive");
    expect(topOf(s).key).not.toBe(first);
  });

  it("an entry remembers the screen it came from, so its back label never changes mid-motion", () => {
    const pushed = planGo(initialStack(archive), doc("a"), { mode: "push", desktop: false });
    expect(pushed.to?.back).toBe("archive");
    const swapped = planGo(pushed.state, doc("b"), { mode: "replace", desktop: false });
    expect(swapped.to?.back).toBe("archive");
    const tab = planGo(swapped.state, settings, { mode: "tab", desktop: false });
    expect(tab.to?.back).toBeNull();
  });

  it("pop takes the top screen off and returns to the one below", () => {
    const s = planGo(initialStack(archive), doc("a"), { mode: "push", desktop: false }).state;
    const plan = planPop(s);
    expect(plan.motion).toBe("pop");
    expect(paths(plan.state)).toEqual(["/"]);
    expect(plan.from?.id).toBe("document");
    expect(plan.to?.id).toBe("archive");
    expect(planPop(plan.state).motion).toBe("none");
  });

  it("the archive's search changes in place: same entry, same key, nothing moves", () => {
    const s = initialStack(archive);
    const plan = planGo(s, at("archive", "/", "?q=passport"), { mode: "replace", desktop: false });
    expect(plan.motion).toBe("none");
    expect(topOf(plan.state).search).toBe("?q=passport");
    expect(topOf(plan.state).key).toBe(topOf(s).key);
  });

  it("the archive under a document keeps its search, and coming back with another one replaces it", () => {
    let s = initialStack(at("archive", "/", "?q=passport"));
    s = planGo(s, doc("a"), { mode: "push", desktop: false }).state;
    expect(s.entries[0]?.search).toBe("?q=passport");
    const plan = planGo(s, at("archive", "/", "?q=visa"), { mode: "push", desktop: false });
    expect(plan.motion).toBe("pop");
    expect(plan.to?.search).toBe("?q=visa");
    expect(topOf(plan.state).key).toBe(s.entries[0]?.key);
  });

  it("withSearch leaves the stack alone when the search is the same", () => {
    const s = initialStack(at("archive", "/", "?q=a"));
    expect(withSearch(s, "/", "?q=a")).toBe(s);
  });
});
