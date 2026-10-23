import { useCallback, useEffect, useRef, useState } from "react";

/** The height the first `rows` rows of a wrapping flex container need. */
export function heightForRows(rowHeight: number, gap: number, rows: number): number {
  return rowHeight * rows + gap * (rows - 1);
}

/**
 * Whether a wrapping list needs folding, and how tall its first rows are.
 *
 * The fold point is geometric, not a count: the same seven tags are two rows in
 * the narrow drawer and one row on the full-width page, and the text-size
 * setting changes it again. So the row height and gap are measured from the
 * rendered DOM.
 *
 * Content height comes from `scrollHeight`, which keeps reporting the full
 * content even while the container is clipped to fewer rows. A ResizeObserver
 * re-measures on width and zoom changes; `watch` re-measures when the content
 * itself changes, since a clipped container does not resize as tags are added
 * past the fold.
 */
export function useRowOverflow(rows = 2, watch = "") {
  const ref = useRef<HTMLDivElement | null>(null);
  const [overflows, setOverflows] = useState(false);
  const [foldedHeight, setFoldedHeight] = useState<number | null>(null);

  const measure = useCallback(() => {
    const node = ref.current;
    const first = node?.firstElementChild as HTMLElement | null | undefined;
    if (!node || !first) {
      setOverflows(false);
      setFoldedHeight(null);
      return;
    }

    const rowGap = parseFloat(getComputedStyle(node).rowGap || "0") || 0;
    const height = heightForRows(first.offsetHeight, rowGap, rows);
    setFoldedHeight(height);
    // Half a pixel of slack: sub-pixel layout should not raise a fold control.
    setOverflows(node.scrollHeight > height + 0.5);
  }, [rows]);

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
  }, [measure, watch]);

  return { ref, overflows, foldedHeight };
}
