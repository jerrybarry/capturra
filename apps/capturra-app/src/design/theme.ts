// The Appearance setting (System / Light / Dark). The Studio's Settings page saves it under
// localStorage "capturra.theme" (as JSON, the same format its helper uses) and sets <html data-theme>.
// Every window calls `followTheme()` once: it applies the saved choice and follows later changes made
// in another window, so the popover and the Studio never disagree. localStorage can be empty or
// unavailable, so every read has a fallback: "system".

export type Theme = "system" | "light" | "dark";
export const THEME_KEY = "capturra.theme";

/** The saved choice; anything missing or unreadable means "system". */
export function parseTheme(raw: string | null): Theme {
  try {
    const value: unknown = JSON.parse(raw ?? "null");
    return value === "light" || value === "dark" ? value : "system";
  } catch {
    return "system";
  }
}

export function readTheme(): Theme {
  try {
    return parseTheme(localStorage.getItem(THEME_KEY));
  } catch {
    return "system";
  }
}

/** "system" removes the attribute, so the stylesheet follows the operating system. */
export function applyTheme(theme: Theme): void {
  const root = document.documentElement;
  if (theme === "light" || theme === "dark") root.dataset.theme = theme;
  else delete root.dataset.theme;
}

export function followTheme(): void {
  applyTheme(readTheme());
  window.addEventListener("storage", (e) => {
    if (e.key === THEME_KEY || e.key === null) applyTheme(readTheme());
  });
}
