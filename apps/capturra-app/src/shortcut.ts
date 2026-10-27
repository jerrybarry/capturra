// Turning a key press into the shortcut string the Rust side registers (Tauri's format, for
// example "CommandOrControl+Shift+2"), and into the symbols macOS shows (⌘⇧2).

const MODIFIER_KEYS = new Set(["Meta", "Control", "Alt", "Shift"]);

/** The key part of a shortcut from `KeyboardEvent.code`, or null for keys we do not accept. */
export function keyName(code: string): string | null {
  const m = /^Key([A-Z])$/.exec(code) ?? /^Digit([0-9])$/.exec(code);
  if (m) return m[1];
  if (/^F([1-9]|1[0-9]|20)$/.test(code)) return code;
  const named: Record<string, string> = { Space: "Space", Enter: "Enter", Tab: "Tab", Minus: "-", Equal: "=", Comma: ",", Period: ".", Slash: "/", Semicolon: ";", Quote: "'", Backquote: "`" };
  return named[code] ?? null;
}

/** A shortcut from a key press: needs a real key plus at least one of Command, Control or Alt. */
export function toShortcut(e: Pick<KeyboardEvent, "code" | "key" | "metaKey" | "ctrlKey" | "altKey" | "shiftKey">): string | null {
  if (MODIFIER_KEYS.has(e.key)) return null; // only a modifier so far
  const key = keyName(e.code);
  if (!key || !(e.metaKey || e.ctrlKey || e.altKey)) return null;
  const parts: string[] = [];
  if (e.metaKey) parts.push("CommandOrControl");
  if (e.ctrlKey) parts.push("Control");
  if (e.altKey) parts.push("Alt");
  if (e.shiftKey) parts.push("Shift");
  parts.push(key);
  return parts.join("+");
}

/** "CommandOrControl+Shift+2" -> "⌘⇧2". */
export function pretty(shortcut: string): string {
  if (!shortcut) return "none";
  const symbols: Record<string, string> = { CommandOrControl: "⌘", Command: "⌘", Control: "⌃", Ctrl: "⌃", Alt: "⌥", Option: "⌥", Shift: "⇧" };
  return shortcut.split("+").map((p) => symbols[p] ?? p).join("");
}
