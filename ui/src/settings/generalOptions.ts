// The choices Settings → General offers for text sizes and zoom.

import { FONT_DEFAULTS, FONT_RANGES, ZOOM_DEFAULT, type FontSizes } from '../shell/theme.ts';

export interface SizeOption {
  value: string;
  title: string;
}

/** Every whole size in the key's range, the default marked. */
export function sizeOptions(key: keyof FontSizes): SizeOption[] {
  const [lo, hi] = FONT_RANGES[key];
  const out: SizeOption[] = [];
  for (let n = lo; n <= hi; n++) out.push({ value: String(n), title: n === FONT_DEFAULTS[key] ? `${n} px (default)` : `${n} px` });
  return out;
}

/** 50% to 200% in steps of 10, the default marked. */
export function zoomOptions(): SizeOption[] {
  const out: SizeOption[] = [];
  for (let n = 50; n <= 200; n += 10) out.push({ value: String(n), title: n === ZOOM_DEFAULT ? `${n}% (default)` : `${n}%` });
  return out;
}

/** The next zoom level one step up (1) or down (-1), kept within 50 to 200. */
export function stepZoom(current: number, dir: 1 | -1): number {
  return Math.min(200, Math.max(50, current + dir * 10));
}
