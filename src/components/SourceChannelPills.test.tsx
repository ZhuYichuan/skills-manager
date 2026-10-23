// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SourceChannelPills } from "./SourceChannelPills";
import { CHANNEL_ORDER } from "../lib/skillChannels";

// The component translates through the same keys My Skills uses, so echoing the
// key back is enough to assert which label each pill got.
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

function render(props: Partial<Parameters<typeof SourceChannelPills>[0]> = {}) {
  act(() => {
    root.render(
      <SourceChannelPills
        channels={[...CHANNEL_ORDER]}
        selected={new Set()}
        onToggle={() => {}}
        {...props}
      />
    );
  });
}

describe("SourceChannelPills", () => {
  it("renders the channels in the order it was given", () => {
    render();
    expect(pills().map((pill) => pill.textContent)).toEqual([
      "mySkills.sourceFilter.local",
      "mySkills.sourceFilter.import",
      "mySkills.sourceFilter.git",
      "mySkills.sourceFilter.skillssh",
    ]);
  });

  it("marks the selected channels, and only those", () => {
    render({ selected: new Set(["git"]) });
    expect(pills().map((pill) => pill.getAttribute("aria-pressed"))).toEqual([
      "false",
      "false",
      "true",
      "false",
    ]);
  });

  it("reports the channel that was clicked", () => {
    const onToggle = vi.fn();
    render({ onToggle });
    act(() => pills()[1].click());
    expect(onToggle).toHaveBeenCalledWith("import");
  });

  it("shows an unknown channel as its raw source_type", () => {
    // Dropping it would look like the filter cannot reach those skills at all.
    render({ channels: [...CHANNEL_ORDER, "well-known"] });
    expect(pills()[4].textContent).toBe("well-known");
  });
});
