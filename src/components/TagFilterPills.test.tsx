// @vitest-environment jsdom
import { act } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { TagFilterPills } from "./TagFilterPills";
import { UNTAGGED_FILTER } from "../lib/skillTags";
import { cleanup, mount, pills } from "../test-utils/mount";

vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));

afterEach(cleanup);

function render(props: Partial<Parameters<typeof TagFilterPills>[0]> = {}) {
  const container = mount(
    <TagFilterPills
      tags={["alpha", "beta"]}
      selected={new Set()}
      onToggle={() => {}}
      onClear={() => {}}
      hasUntagged
      {...props}
    />
  );
  return pills(container);
}

const texts = (buttons: HTMLButtonElement[]) => buttons.map((pill) => pill.textContent);

describe("TagFilterPills", () => {
  it("offers the all-tags pill first, then the tags themselves", () => {
    expect(texts(render())).toEqual([
      "mySkills.tags.allTags",
      "mySkills.tags.untagged",
      "alpha",
      "beta",
    ]);
  });

  it("hides the untagged pill when every skill is tagged", () => {
    // An untagged pill over a list with no untagged skill filters to nothing.
    expect(texts(render({ hasUntagged: false }))).toEqual([
      "mySkills.tags.allTags",
      "alpha",
      "beta",
    ]);
  });

  it("marks the all-tags pill while no tag filter is set", () => {
    expect(render()[0].getAttribute("aria-pressed")).toBe("true");
    cleanup();
    const oneSelected = render({ selected: new Set(["alpha"]) });
    expect(oneSelected[0].getAttribute("aria-pressed")).toBe("false");
    expect(oneSelected[2].getAttribute("aria-pressed")).toBe("true");
  });

  it("clears through the all-tags pill and toggles a tag through its own", () => {
    const onClear = vi.fn();
    const onToggle = vi.fn();
    const buttons = render({ onClear, onToggle });
    act(() => buttons[0].click());
    act(() => buttons[3].click());
    expect(onClear).toHaveBeenCalledTimes(1);
    expect(onToggle).toHaveBeenCalledWith("beta");
  });

  it("toggles the untagged filter through the same callback", () => {
    const onToggle = vi.fn();
    const shown = render({ onToggle });
    act(() => shown[1].click());
    expect(onToggle).toHaveBeenCalledWith(UNTAGGED_FILTER);
  });

  it("only opens the tag menu where management is offered", () => {
    const onTagContextMenu = vi.fn();
    const plain = render();
    act(() => {
      plain[2].dispatchEvent(new MouseEvent("contextmenu", { bubbles: true }));
    });
    expect(onTagContextMenu).not.toHaveBeenCalled();

    cleanup();
    const managed = render({ onTagContextMenu });
    act(() => {
      managed[2].dispatchEvent(new MouseEvent("contextmenu", { bubbles: true }));
    });
    expect(onTagContextMenu).toHaveBeenCalledWith("alpha", expect.anything());
  });
});
