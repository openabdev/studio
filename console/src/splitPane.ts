// Drag-resize for the drilldown row's two columns (main content ↔ the
// persistent Agent-chat side, `.drilldown-side`). This module owns the pure
// clamp/persistence logic; the mousedown/move/up DOM wiring lives in main.ts
// (same split as theme.ts/theme toggle).

const STORAGE_KEY = "oab-studio.drilldown-side-width";
// Brett: wanted more adjustment budget than 280–900 — widened both ends.
// `.drilldown-main` gets its own 360px floor (styles.css) so the left column
// can't get crushed unreadable when the side is dragged near MAX_WIDTH.
export const MIN_WIDTH = 220;
export const MAX_WIDTH = 1200;
export const DEFAULT_WIDTH = 420;

// Pure: clamp a candidate side-column width (px) into the allowed range.
export function clampWidth(px: number): number {
  if (!Number.isFinite(px)) return DEFAULT_WIDTH;
  return Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, Math.round(px)));
}

// Read the saved width, tolerating a missing/garbage value.
export function readWidth(): number {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    const n = raw === null ? NaN : Number(raw);
    return clampWidth(n);
  } catch {
    return DEFAULT_WIDTH;
  }
}

export function saveWidth(px: number): void {
  try {
    localStorage.setItem(STORAGE_KEY, String(clampWidth(px)));
  } catch {
    /* storage unavailable — in-memory only for this session */
  }
}

// Wire the drag handle: apply the saved width up front, then drag the handle
// left/right to grow/shrink `side` (it sits to the handle's right), clamping
// live and persisting once on release.
export function initSplitPane(handle: HTMLElement, side: HTMLElement): void {
  side.style.setProperty("--drilldown-side-w", `${readWidth()}px`);

  let startX = 0;
  let startWidth = 0;

  const onMove = (e: MouseEvent): void => {
    const next = clampWidth(startWidth - (e.clientX - startX));
    side.style.setProperty("--drilldown-side-w", `${next}px`);
  };
  const onUp = (e: MouseEvent): void => {
    document.removeEventListener("mousemove", onMove);
    document.removeEventListener("mouseup", onUp);
    handle.classList.remove("is-dragging");
    saveWidth(clampWidth(startWidth - (e.clientX - startX)));
  };
  handle.addEventListener("mousedown", (e) => {
    startX = e.clientX;
    startWidth = side.getBoundingClientRect().width;
    handle.classList.add("is-dragging");
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
    e.preventDefault();
  });
}
