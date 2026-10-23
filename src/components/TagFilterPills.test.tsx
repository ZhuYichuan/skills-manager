// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { TagFilterPills } from "./TagFilterPills";
import { UNTAGGED_FILTER } from "../lib/skillTags";

vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

const pills = () => Array.from(container.querySelectorAll("button"));
const texts = () => pills().map((pill) => pill.textContent);

function render(props: Partial<Parameters<typeof TagFilterPills>[0]> = {}) {
  act(() => {
    root.render(
      <TagFilterPills
        tags={["alpha", "beta"]}
        selected={new Set()}
        onToggle={() => {}}
        onClear={() => {}}
        hasUntagged
        {...props}
      />
    );
  });
}

describe("TagFilterPills", () => {
  it("offers the all-tags pill first, then the tags themselves", () => {
    render();
    expect(texts()).toEqual([
      "mySkills.tags.allTags",
      "mySkills.tags.untagged",
      "alpha",
      "beta",
    ]);
  });

  it("hides the untagged pill when every skill is tagged", () => {
    // An untagged pill over a list with no untagged skill filters to nothing.
    render({ hasUntagged: false });
    expect(texts()).toEqual(["mySkills.tags.allTags", "alpha", "beta"]);
  });

  it("marks the all-tags pill while no tag filter is set", () => {
    render();
    expect(pills()[0].getAttribute("aria-pressed")).toBe("true");
    act(() => root.unmount());
    root = createRoot(container);
    render({ selected: new Set(["alpha"]) });
    expect(pills()[0].getAttribute("aria-pressed")).toBe("false");
    expect(pills()[2].getAttribute("aria-pressed")).toBe("true");
  });

  it("clears through the all-tags pill and toggles a tag through its own", () => {
    const onClear = vi.fn();
    const onToggle = vi.fn();
    render({ onClear, onToggle });
    act(() => pills()[0].click());
    act(() => pills()[3].click());
    expect(onClear).toHaveBeenCalledTimes(1);
    expect(onToggle).toHaveBeenCalledWith("beta");
  });

  it("toggles the untagged filter through the same callback", () => {
    const onToggle = vi.fn();
    render({ onToggle });
    act(() => pills()[1].click());
    expect(onToggle).toHaveBeenCalledWith(UNTAGGED_FILTER);
  });

  it("only opens the tag menu where management is offered", () => {
    const onTagContextMenu = vi.fn();
    render();
    act(() => {
      pills()[2].dispatchEvent(new MouseEvent("contextmenu", { bubbles: true }));
    });
    expect(onTagContextMenu).not.toHaveBeenCalled();

    act(() => root.unmount());
    root = createRoot(container);
    render({ onTagContextMenu });
    act(() => {
      pills()[2].dispatchEvent(new MouseEvent("contextmenu", { bubbles: true }));
    });
    expect(onTagContextMenu).toHaveBeenCalledWith("alpha", expect.anything());
  });
});
