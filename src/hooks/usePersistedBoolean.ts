import { useCallback, useState } from "react";

/**
 * A boolean view preference that survives a reload, stored as "true" / "false".
 *
 * The read happens in the state initializer and the write in the setter rather
 * than in an effect, so the stored value is never a render behind. Storage is
 * optional: when the browser refuses it (private mode, storage disabled) the
 * value still works for the session.
 */
export function usePersistedBoolean(
  key: string,
  fallback: boolean
): [boolean, (next: boolean) => void] {
  const [value, setValueState] = useState<boolean>(() => {
    try {
      const stored = localStorage.getItem(key);
      if (stored === "true") return true;
      if (stored === "false") return false;
    } catch {
      // Fall through to the caller's default.
    }
    return fallback;
  });

  const setValue = useCallback(
    (next: boolean) => {
      setValueState(next);
      try {
        localStorage.setItem(key, String(next));
      } catch {
        // The value still works for the current session.
      }
    },
    [key]
  );

  return [value, setValue];
}
