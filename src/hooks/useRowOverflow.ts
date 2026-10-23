import { useCallback, useEffect, useRef, useState } from "react";

/** How many rows stay visible before the tail folds away. */
export const FOLDED_ROWS = 2;

/** The height the first `rows` rows of a wrapping flex container need. */
export function heightForRows(rowHeight: number, gap: number, rows: number): number {
  return rowHeight * rows + gap * (rows - 1);
}

export interface RowFold {
  /** Height of the rows that stay visible; 0 until measured. */
  height: number;
  /** Index of the first child that falls below the fold, or null when nothing does. */
  firstFoldedIndex: number | null;
}

const NOTHING_FOLDED: RowFold = { height: 0, firstFoldedIndex: null };

/**
 * Where a wrapping list has to fold, measured from the rendered DOM.
 *
 * The fold point is geometric, not a count: the same seven tags are two rows in
 * the narrow drawer and one row on the full-width page, and the text-size
 * setting moves it again. Per-child geometry (rather than a single overflow
 * test) is what lets the caller keep the visible part interactive and fold only
 * the tail.
 *
 * A ResizeObserver re-measures on width and zoom changes. `contentKey` covers
 * the other case: a clipped container does not resize as children are added
 * past the fold, so the content itself has to trigger a re-measure.
 */
export function useRowOverflow(contentKey: string) {
  const ref = useRef<HTMLDivElement | null>(null);
  const [fold, setFold] = useState<RowFold>(NOTHING_FOLDED);

  const measure = useCallback(() => {
    const node = ref.current;
    const children = node ? (Array.from(node.children) as HTMLElement[]) : [];
    if (!node || children.length === 0) {
      setFold(NOTHING_FOLDED);
      return;
    }

    const rowGap = parseFloat(getComputedStyle(node).rowGap || "0") || 0;
    const height = heightForRows(children[0].offsetHeight, rowGap, FOLDED_ROWS);
    // Children flow in order, so the first one whose centre sits below the fold
    // bounds everything that follows.
    const top = node.getBoundingClientRect().top;
    const firstFoldedIndex = children.findIndex((child) => {
      const rect = child.getBoundingClientRect();
      return rect.top + rect.height / 2 > top + height;
    });

    setFold({
      height,
      firstFoldedIndex: firstFoldedIndex === -1 ? null : firstFoldedIndex,
    });
  }, []);

  useEffect(() => {
    const node = ref.current;
    if (!node) return;

    if (typeof ResizeObserver === "undefined") {
      const frame = requestAnimationFrame(measure);
      return () => cancelAnimationFrame(frame);
    }

    // observe() delivers one entry immediately, which is the first measurement.
    const observer = new ResizeObserver(measure);
    observer.observe(node);
    return () => observer.disconnect();
  }, [measure, contentKey]);

  return { ref, ...fold };
}
