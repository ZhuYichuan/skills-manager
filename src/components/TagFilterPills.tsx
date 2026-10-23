import type { MouseEvent } from "react";
import { CircleSlash } from "lucide-react";
import { useTranslation } from "react-i18next";
import { cn } from "../utils";
import {
  getTagActiveColor,
  getTagColor,
  UNTAGGED_FILTER,
} from "../lib/skillTags";

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

/**
 * The tag half of a skill filter bar. My Skills and the drawer that adds skills
 * to an Agent or a project render this, so a tag looks and behaves the same in
 * both: same order, same colours, same "all tags" and "untagged" pills.
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
  const everythingSelected = selected.size === 0;

  return (
    <div className={cn("flex flex-wrap items-center gap-1.5", className)}>
      {label && <span className="text-[12px] text-muted">{label}</span>}
      <button
        type="button"
        aria-pressed={everythingSelected}
        onClick={onClear}
        className={cn(
          "rounded-full px-2.5 py-0.5 text-[12px] font-medium transition-colors",
          everythingSelected
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
  );
}
