import { C } from "./capturra";

/** Ask for a new title. Resolves with the trimmed text, or `null` if cancelled or unchanged. */
export function promptRename(current: string): Promise<string | null> {
  return new Promise((resolve) => {
    let result: string | null = null;
    const sheet = C().sheet({
      title: "Rename recording",
      className: "narrow",
      body: `<div class="field"><div class="lbl"><label for="rename-title">Title</label></div>
        <input class="input" id="rename-title" type="text" maxlength="200" spellcheck="false" value="${C().esc(current)}"></div>`,
      footer: `<button class="btn ghost" data-cancel>Cancel</button><button class="btn primary" data-save>Rename</button>`,
      onClose: () => resolve(result),
    });
    const input = sheet.el("#rename-title") as HTMLInputElement;
    const save = () => {
      const text = input.value.trim();
      if (!text || text === current) return sheet.close();
      result = text;
      sheet.close();
    };
    sheet.el("[data-save]")?.addEventListener("click", save);
    sheet.el("[data-cancel]")?.addEventListener("click", () => sheet.close());
    input.addEventListener("keydown", (e) => { if (e.key === "Enter") save(); });
    input.focus();
    input.select();
  });
}
