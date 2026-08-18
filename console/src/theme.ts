// Appearance toggle. Studio already renders light + dark via
// `prefers-color-scheme` (auto-follows the OS); this adds a manual override so
// the operator can pin an appearance. The choice lives on
// `document.documentElement[data-theme]` — "light"/"dark" force a palette, and
// its absence ("system") lets the OS media query decide (see styles.css). The
// no-FOUC head script in index.html applies the saved choice before first paint;
// this module owns the runtime toggle + persistence.

export type Theme = "system" | "light" | "dark";

const STORAGE_KEY = "oab-studio.theme";
const ORDER: readonly Theme[] = ["system", "light", "dark"];
const LABEL: Record<Theme, string> = {
  system: "System",
  light: "Light",
  dark: "Dark",
};

// Pure: the next appearance in the System → Light → Dark → System cycle. An
// unknown value falls back into the cycle at System.
export function cycleTheme(current: Theme): Theme {
  const i = ORDER.indexOf(current);
  return ORDER[(i + 1) % ORDER.length] ?? "system";
}

// Read the saved choice, tolerating a missing/garbage value (⇒ "system").
export function readTheme(): Theme {
  try {
    const t = localStorage.getItem(STORAGE_KEY);
    if (t === "light" || t === "dark" || t === "system") return t;
  } catch {
    /* storage unavailable — fall through to system */
  }
  return "system";
}

// Reflect a choice onto the document: "system" removes the attribute so the
// prefers-color-scheme media query applies; otherwise pin the palette.
export function applyTheme(theme: Theme): void {
  const root = document.documentElement;
  if (theme === "system") root.removeAttribute("data-theme");
  else root.setAttribute("data-theme", theme);
}

// Wire the toolbar button: show the current choice, and cycle + persist + apply
// on click. Applies the saved choice up front so the button and the document
// agree even if the head script didn't run (e.g. tests / SSR).
export function initThemeToggle(btn: HTMLButtonElement): void {
  let current = readTheme();
  const paint = (): void => {
    btn.textContent = LABEL[current];
  };
  applyTheme(current);
  paint();
  btn.addEventListener("click", () => {
    current = cycleTheme(current);
    try {
      localStorage.setItem(STORAGE_KEY, current);
    } catch {
      /* storage unavailable — the choice still applies for this session */
    }
    applyTheme(current);
    paint();
  });
}
