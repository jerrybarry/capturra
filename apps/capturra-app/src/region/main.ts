// The region picker window: a dimmed screenshot of the display and a rectangle to drag over it.
// The Rust side (`region_picker.rs`) owns the screenshot and snaps the final numbers.
import { invoke } from "@tauri-apps/api/core";
import "../design/tokens.css";
import "../design/components.css";
import "./region.css";
import { bigEnough, boxBetween, clampTo, sizeLabel, toDisplay, type Point } from "./geometry";

interface PickerInfo { image: string; width: number; height: number; offset_x: number; offset_y: number }

const el = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const [shot, dim, sel, size, bar, hint] = [el<HTMLImageElement>("shot"), el("dim"), el("sel"), el("size"), el("bar"), el("hint")];

let info: PickerInfo;
let anchor: Point | null = null; // where the drag began, in display points
let current: Point | null = null;
let busy = false;

const offset = (): Point => ({ x: info.offset_x, y: info.offset_y });
const onDisplay = (e: PointerEvent): Point => clampTo(toDisplay({ x: e.clientX, y: e.clientY }, offset()), info.width, info.height);

function draw() {
  if (!anchor || !current) return;
  const box = boxBetween(anchor, current);
  dim.hidden = true;
  sel.hidden = false;
  Object.assign(sel.style, { left: `${box.x - info.offset_x}px`, top: `${box.y - info.offset_y}px`, width: `${box.w}px`, height: `${box.h}px` });
  size.textContent = sizeLabel(box);
  // keep the size label inside the picture when the box is at the top edge
  size.classList.toggle("below", box.y - info.offset_y < 28);
}

function say(text: string, problem = false) {
  hint.textContent = text;
  hint.classList.toggle("problem", problem);
}

async function submit() {
  if (busy || !anchor || !current) return;
  busy = true;
  try {
    await invoke("picker_submit", { a: [anchor.x, anchor.y], b: [current.x, current.y] });
  } catch (e) {
    say(String(e), true); // too small: stay open so the user can drag again
    busy = false;
  }
}

const cancel = () => void invoke("picker_cancel");

window.addEventListener("pointerdown", (e) => {
  if ((e.target as HTMLElement).closest("#bar")) return;
  anchor = current = onDisplay(e);
  bar.hidden = true;
  say("Drag to select what to record");
  document.body.setPointerCapture?.(e.pointerId);
});
window.addEventListener("pointermove", (e) => {
  if (!anchor || (e.buttons & 1) === 0) return;
  current = onDisplay(e);
  draw();
});
window.addEventListener("pointerup", (e) => {
  if (!anchor || !current) return;
  current = onDisplay(e);
  draw();
  if (bigEnough(boxBetween(anchor, current))) {
    bar.hidden = false;
    say("Drag again to change it");
  } else {
    sel.hidden = true;
    dim.hidden = false;
    anchor = current = null;
    say("That is too small. Drag a bigger area", true);
  }
});
window.addEventListener("keydown", (e) => {
  if (e.key === "Escape") cancel();
  else if (e.key === "Enter") void submit();
});
el("use").addEventListener("click", () => void submit());
el("cancel").addEventListener("click", cancel);

async function start() {
  info = await invoke<PickerInfo>("picker_info");
  // Draw the screenshot at display size, shifted by however far the window sits from the display's corner.
  Object.assign(shot.style, { width: `${info.width}px`, height: `${info.height}px`, left: `${-info.offset_x}px`, top: `${-info.offset_y}px` });
  shot.src = info.image;
  window.focus();
}

start().catch((e) => say(`Could not start the picker: ${e}`, true));
