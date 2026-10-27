// Pure geometry for the region picker page.

export interface Point { x: number; y: number }
export interface Box { x: number; y: number; w: number; h: number }

/** The smallest drag the recorder will take, in points (matches `MIN_SIZE` in region_picker.rs). */
export const MIN_SIZE = 16;

/** A point on the page -> the same point on the display. The page can sit below the menu bar. */
export function toDisplay(page: Point, offset: Point): Point {
  return { x: page.x + offset.x, y: page.y + offset.y };
}

/** The box between two corners, whichever way the drag went. */
export function boxBetween(a: Point, b: Point): Box {
  return { x: Math.min(a.x, b.x), y: Math.min(a.y, b.y), w: Math.abs(a.x - b.x), h: Math.abs(a.y - b.y) };
}

/** Keep a point inside the display. */
export function clampTo(p: Point, width: number, height: number): Point {
  return { x: Math.min(Math.max(p.x, 0), width), y: Math.min(Math.max(p.y, 0), height) };
}

export const bigEnough = (b: Box): boolean => b.w >= MIN_SIZE && b.h >= MIN_SIZE;

/** "1280 × 720", in whole points. */
export const sizeLabel = (b: Box): string => `${Math.round(b.w)} × ${Math.round(b.h)}`;
