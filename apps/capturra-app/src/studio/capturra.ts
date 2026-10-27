// The slice of the shared `window.Capturra` object (see ui.ts and bridge.ts) that the typed
// Studio modules use. The older modules are untyped ports and keep using it directly.

export interface Sheet {
  root: HTMLElement;
  el(selector: string): HTMLElement | null;
  all(selector: string): HTMLElement[];
  close(): void;
}

export interface SheetOptions { title: string; body?: string; footer?: string; className?: string; onClose?: () => void }

export interface CapturraGlobal {
  isTauri: boolean;
  invoke<T = unknown>(command: string, args?: Record<string, unknown>): Promise<T>;
  /** Per-viewer conveniences kept in localStorage; may be empty or unavailable, so always give a fallback. */
  store: { get<T>(key: string, fallback: T): T; set(key: string, value: unknown): void };
  theme: { apply(theme: string): void };
  sheet(options: SheetOptions): Sheet;
  toast(message: string, options?: { error?: boolean; duration?: number }): void;
  esc(text: string): string;
}

export const C = (): CapturraGlobal => (window as unknown as { Capturra: CapturraGlobal }).Capturra;
