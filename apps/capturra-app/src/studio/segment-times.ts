// Time values in a project are whole milliseconds. The timeline measures fractional ones while an
// edge is being dragged, so everything is rounded before it is saved (the app rounds again, so a
// stray fraction can never cost an edit).

export interface TimedSegment {
  t0: number;
  p0: number;
  p1: number;
  t1: number;
  focus: { t: number }[];
}

/** A time as whole, non-negative milliseconds. */
export const wholeMs = (value: number): number => (Number.isFinite(value) ? Math.max(0, Math.round(value)) : 0);

/** Copies of the segments with every time field in whole milliseconds; other fields are untouched. */
export function withWholeMs<T extends TimedSegment>(segments: T[]): T[] {
  return segments.map((s) => ({
    ...s,
    t0: wholeMs(s.t0),
    p0: wholeMs(s.p0),
    p1: wholeMs(s.p1),
    t1: wholeMs(s.t1),
    focus: s.focus.map((f) => ({ ...f, t: wholeMs(f.t) })),
  }));
}

/** The trim range in whole milliseconds. */
export const trimWholeMs = (trim: { in: number; out: number }): { in: number; out: number } => ({ in: wholeMs(trim.in), out: wholeMs(trim.out) });
