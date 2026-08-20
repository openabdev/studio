// Drag-resize between the drilldown row (Fleets/chat) and whatever sits below
// it — today just the fleets.toml/agents.toml editor, the only other
// standing section. Shown only while that's open: with nothing below there's
// nothing to divide the row's height against, so the row just auto-fills the
// window instead (`main.ts`'s `syncDrilldownHeight`). Same pure
// clamp/persist + DOM-wiring split as splitPane.ts (the left/right version).

const STORAGE_KEY = "oab-studio.drilldown-row-height";
export const MIN_HEIGHT = 160;
export const MAX_HEIGHT = 2000;
export const DEFAULT_HEIGHT = 360;

// Pure: clamp a candidate row height (px) into the allowed range.
export function clampHeight(px: number): number {
  if (!Number.isFinite(px)) return DEFAULT_HEIGHT;
  return Math.min(MAX_HEIGHT, Math.max(MIN_HEIGHT, Math.round(px)));
}

// Read the saved height, tolerating a missing/garbage value.
export function readHeight(): number {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    const n = raw === null ? NaN : Number(raw);
    return clampHeight(n);
  } catch {
    return DEFAULT_HEIGHT;
  }
}

export function saveHeight(px: number): void {
  try {
    localStorage.setItem(STORAGE_KEY, String(clampHeight(px)));
  } catch {
    /* storage unavailable — in-memory only for this session */
  }
}

// Apply the saved (or default) height to `row` — called whenever the editor
// opens, switching the row from auto-fill into its fixed, draggable mode.
// `onChange` is the caller's hook to keep dependent layout (the editor's own
// fill height) in sync with the row's new size.
export function applySavedRowHeight(row: HTMLElement, onChange: () => void): void {
  row.style.setProperty("--drilldown-row-h", `${readHeight()}px`);
  onChange();
}

// Wire the drag handle: drag up/down to shrink/grow `row` (it sits above the
// handle), clamping live and persisting once on release. `onChange` runs on
// every live update too, not just at the end — the editor below needs to
// reflow as the row's height changes, not just once it settles.
export function initRowResize(handle: HTMLElement, row: HTMLElement, onChange: () => void): void {
  let startY = 0;
  let startHeight = 0;

  const onMove = (e: MouseEvent): void => {
    const next = clampHeight(startHeight + (e.clientY - startY));
    row.style.setProperty("--drilldown-row-h", `${next}px`);
    onChange();
  };
  const onUp = (e: MouseEvent): void => {
    document.removeEventListener("mousemove", onMove);
    document.removeEventListener("mouseup", onUp);
    handle.classList.remove("is-dragging");
    saveHeight(clampHeight(startHeight + (e.clientY - startY)));
  };
  handle.addEventListener("mousedown", (e) => {
    startY = e.clientY;
    startHeight = row.getBoundingClientRect().height;
    handle.classList.add("is-dragging");
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
    e.preventDefault();
  });
}
