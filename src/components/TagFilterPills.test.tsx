// @vitest-environment jsdom
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { UNTAGGED_FILTER } from "../lib/skillTags";
import { cleanup, mount, pills } from "../test-utils/mount";

// jsdom reports every height as 0, so the fold decision — a geometric one —
// cannot be measured here. It is mocked; the presentation it drives is not.
const overflow = vi.hoisted(() => ({ overflows: false, foldedHeight: 40 }));
vi.mock("../hooks/useRowOverflow", () => ({
  useRowOverflow: () => ({
    ref: { current: null },
    overflows: overflow.overflows,
    foldedHeight: overflow.foldedHeight,
  }),
}));

vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));

const { TagFilterPills } = await import("./TagFilterPills");

beforeEach(() => {
  overflow.overflows = false;
  overflow.foldedHeight = 40;
});

afterEach(cleanup);

const tags = ["alpha", "beta", "gamma"];

function render(props: Partial<Parameters<typeof TagFilterPills>[0]> = {}) {
  const container = mount(
    <TagFilterPills
      tags={tags}
      selected={new Set()}
      onToggle={() => {}}
      onClear={() => {}}
      hasUntagged
      {...props}
    />
  );
  const buttons = () => pills(container);
  const foldControl = () =>
    buttons().find((button) => button.getAttribute("aria-expanded") !== null);
  const folded = () => container.querySelector("[inert]") as HTMLElement | null;
  return { container, buttons, foldControl, folded };
}

describe("TagFilterPills", () => {
  it("offers the all-tags pill first, then the tags themselves", () => {
    expect(render().buttons().map((pill) => pill.textContent)).toEqual([
      "mySkills.tags.allTags",
      "mySkills.tags.untagged",
      "alpha",
      "beta",
      "gamma",
    ]);
  });

  it("hides the untagged pill when every skill is tagged", () => {
    // An untagged pill over a list with no untagged skill filters to nothing.
    expect(render({ hasUntagged: false }).buttons()).toHaveLength(4);
  });

  it("marks the all-tags pill while no tag filter is set", () => {
    expect(render().buttons()[0].getAttribute("aria-pressed")).toBe("true");
    cleanup();
    expect(render({ selected: new Set(["alpha"]) }).buttons()[0].getAttribute("aria-pressed")).toBe(
      "false"
    );
  });

  it("clears through the all-tags pill and toggles a tag through its own", () => {
    const onClear = vi.fn();
    const onToggle = vi.fn();
    const shown = render({ onClear, onToggle });
    act(() => shown.buttons()[0].click());
    act(() => shown.buttons()[3].click());
    expect(onClear).toHaveBeenCalledTimes(1);
    expect(onToggle).toHaveBeenCalledWith("beta");
  });

  it("toggles the untagged filter through the same callback", () => {
    const onToggle = vi.fn();
    const shown = render({ onToggle });
    act(() => shown.buttons()[1].click());
    expect(onToggle).toHaveBeenCalledWith(UNTAGGED_FILTER);
  });

  it("only opens the tag menu where management is offered", () => {
    const onTagContextMenu = vi.fn();
    const plain = render();
    act(() => {
      plain.buttons()[2].dispatchEvent(new MouseEvent("contextmenu", { bubbles: true }));
    });
    expect(onTagContextMenu).not.toHaveBeenCalled();

    cleanup();
    const managed = render({ onTagContextMenu });
    act(() => {
      managed.buttons()[2].dispatchEvent(new MouseEvent("contextmenu", { bubbles: true }));
    });
    expect(onTagContextMenu).toHaveBeenCalledWith("alpha", expect.anything());
  });

  it("offers no fold control when the tags fit", () => {
    expect(render().foldControl()).toBeUndefined();
    expect(render().folded()).toBeNull();
  });

  it("folds the tail behind a control that says how many there are", () => {
    overflow.overflows = true;
    const shown = render();
    expect(shown.foldControl()?.textContent).toBe("mySkills.tags.showAll");
    expect(shown.foldControl()?.getAttribute("aria-expanded")).toBe("false");
  });

  it("keeps the folded tags out of the tab order while folded", () => {
    overflow.overflows = true;
    expect(render().folded()).not.toBeNull();
  });

  it("expands on click and offers to fold again", () => {
    overflow.overflows = true;
    const shown = render();
    act(() => shown.foldControl()?.click());
    expect(shown.foldControl()?.getAttribute("aria-expanded")).toBe("true");
    expect(shown.foldControl()?.textContent).toBe("mySkills.tags.showFewer");
    expect(shown.folded()).toBeNull();
  });

  it("says how many folded tags are still filtering the list", () => {
    // A filter the user cannot see still narrows the list; the control admits it.
    overflow.overflows = true;
    const shown = render({ selected: new Set(["beta", "gamma"]) });
    expect(shown.foldControl()?.textContent).toBe(
      "mySkills.tags.showAll mySkills.tags.foldedSelected"
    );
  });

  it("drops that note once expanded, since the tags are visible again", () => {
    overflow.overflows = true;
    const shown = render({ selected: new Set(["beta"]) });
    act(() => shown.foldControl()?.click());
    expect(shown.foldControl()?.textContent).toBe("mySkills.tags.showFewer");
  });

  it("never folds the all-tags or untagged pills", () => {
    overflow.overflows = true;
    const { container } = render();
    const folded = container.querySelector("[inert]") as HTMLElement;
    const foldedText = folded.textContent ?? "";
    expect(foldedText).not.toContain("mySkills.tags.allTags");
    expect(foldedText).not.toContain("mySkills.tags.untagged");
  });
});
