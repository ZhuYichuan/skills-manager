import { useState, type MouseEvent } from "react";
import { CircleSlash } from "lucide-react";
import { useTranslation } from "react-i18next";
import { cn } from "../utils";
import {
  getTagActiveColor,
  getTagColor,
  UNTAGGED_FILTER,
} from "../lib/skillTags";
import { useRowOverflow } from "../hooks/useRowOverflow";

interface Props {
  /** Every tag present in the list being filtered, in display order. */
  tags: string[];
  selected: ReadonlySet<string>;
  onToggle: (tag: string) => void;
  /** Clears the tag filter: the "all tags" pill. */
  onClear: () => void;
  /** Whether any skill in the list carries no tags, which is what the untagged pill filters. */
  hasUntagged: boolean;
  /**
   * Right-click on a tag. Omitted where tag management is not offered (the
   * drawer that adds skills to an Agent or a project only filters).
   */
  onTagContextMenu?: (tag: string, event: MouseEvent) => void;
  /** Optional caption for a column layout. A single filter row reads fine without one. */
  label?: string;
  className?: string;
}

/** How many rows of tags stay visible before the rest fold away. */
const VISIBLE_ROWS = 2;

/**
 * The tag half of a skill filter bar. My Skills and the drawer that adds skills
 * to an Agent or a project render this, so a tag looks and behaves the same in
 * both: same order, same colours, same "all tags" and "untagged" pills.
 *
 * A library with fifty tags would otherwise push the list it filters off the
 * screen, so anything past two rows folds behind one control. "All tags" and
 * "untagged" never fold: they are what tells the user no tag filter is set.
 */
export function TagFilterPills({
  tags,
  selected,
  onToggle,
  onClear,
  hasUntagged,
  onTagContextMenu,
  label,
  className,
}: Props) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  // A clipped container does not resize when tags are added past the fold, so
  // the content itself is the trigger here.
  const { ref, overflows, foldedHeight } = useRowOverflow(VISIBLE_ROWS, tags.join("\u0000"));
  const folded = overflows && !expanded;
  // Tags the fold is hiding that still filter the list — the control has to
  // admit they exist, or the user has no way to find them again.
  const selectedInFoldedRows = folded
    ? tags.filter((tag) => selected.has(tag)).length
    : 0;

  return (
    <div className={cn("flex flex-wrap items-start gap-1.5", className)}>
      <div className="flex flex-wrap items-center gap-1.5">
        {label && <span className="text-[12px] text-muted">{label}</span>}
        <button
          type="button"
          aria-pressed={selected.size === 0}
          onClick={onClear}
          className={cn(
            "rounded-full px-2.5 py-0.5 text-[12px] font-medium transition-colors",
            selected.size === 0
              ? "bg-accent text-white dark:bg-accent dark:text-white"
              : "bg-surface-hover text-muted hover:text-secondary"
          )}
        >
          {t("mySkills.tags.allTags")}
        </button>
        {hasUntagged && (
          <button
            type="button"
            aria-pressed={selected.has(UNTAGGED_FILTER)}
            onClick={() => onToggle(UNTAGGED_FILTER)}
            title={t("mySkills.tags.untagged")}
            className={cn(
              "inline-flex items-center gap-1 rounded-full px-2.5 py-0.5 text-[12px] font-medium transition-colors",
              selected.has(UNTAGGED_FILTER)
                ? "bg-surface-active text-primary"
                : "border border-dashed border-border text-muted hover:text-secondary"
            )}
          >
            <CircleSlash className="h-3 w-3" />
            {t("mySkills.tags.untagged")}
          </button>
        )}
      </div>

      <div
        ref={ref}
        // Folding is a visual cut, not a removal: without this the clipped tags
        // stay in the tab order and keyboard users land on buttons they cannot
        // see.
        inert={folded}
        style={
          folded && foldedHeight !== null
            ? {
                maxHeight: foldedHeight,
                WebkitMaskImage: "linear-gradient(to bottom, black 60%, transparent)",
                maskImage: "linear-gradient(to bottom, black 60%, transparent)",
              }
            : undefined
        }
        className={cn(
          "flex min-w-0 flex-1 flex-wrap items-center gap-1.5",
          folded && "overflow-hidden"
        )}
      >
        {tags.map((tag) => {
          const active = selected.has(tag);
          return (
            <button
              key={tag}
              type="button"
              aria-pressed={active}
              onClick={() => onToggle(tag)}
              onContextMenu={
                onTagContextMenu
                  ? (event) => {
                      event.preventDefault();
                      onTagContextMenu(tag, event);
                    }
                  : undefined
              }
              title={onTagContextMenu ? t("mySkills.tags.manageHint") : undefined}
              className={cn(
                "rounded-full px-2.5 py-0.5 text-[12px] font-medium transition-colors",
                active ? getTagActiveColor(tag, tags) : getTagColor(tag, tags)
              )}
            >
              {tag}
            </button>
          );
        })}
      </div>

      {overflows && (
        <button
          type="button"
          aria-expanded={expanded}
          onClick={() => setExpanded((wasExpanded) => !wasExpanded)}
          className="rounded-md px-1 py-0.5 text-[12px] font-medium text-muted transition-colors hover:text-secondary"
        >
          {expanded
            ? t("mySkills.tags.showFewer")
            : t("mySkills.tags.showAll", { count: tags.length })}
          {selectedInFoldedRows > 0 &&
            ` ${t("mySkills.tags.foldedSelected", { count: selectedInFoldedRows })}`}
        </button>
      )}
    </div>
  );
}
