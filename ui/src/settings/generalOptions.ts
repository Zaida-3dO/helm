// The choices Settings → General offers for text sizes, zoom and the interface skin.

import { FONT_DEFAULTS, FONT_RANGES, SKINS, ZOOM_DEFAULT, type FontSizes, type Skin } from '../shell/theme.ts';
import { SOUNDS_DEFAULT } from '../shell/sounds.ts';

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

const SKIN_TITLES: Record<Skin, string> = {
  default: 'Default',
  'windows-xp': 'Windows XP',
  slack: 'Slack',
  chrome: 'Chrome',
  terminal: 'Terminal',
  intellij: 'IntelliJ',
  discord: 'Discord',
  reddit: 'Reddit',
  wikipedia: 'Wikipedia',
};

/** Every skin styles.css carries, in the order Settings shows them; the default marked. */
export function skinOptions(): SizeOption[] {
  return SKINS.map((s) => ({
    value: s,
    title: s === 'default' ? 'Default (no skin)' : SKIN_TITLES[s],
  }));
}

/** The three delight levels Settings offers (a custom per-effect set is not editable yet). */
export function motionOptions(): SizeOption[] {
  return [
    { value: 'zen', title: 'Quiet' },
    { value: 'standard', title: 'Standard (default)' },
    { value: 'brainrot', title: 'Maximal' },
  ];
}

/** Sound level 0 to 100 in steps of 5, the default marked and 0 named. */
export function volumeOptions(): SizeOption[] {
  const out: SizeOption[] = [];
  for (let n = 0; n <= 100; n += 5) {
    const title = n === SOUNDS_DEFAULT.volume ? `${n}% (default)` : n === 0 ? 'Silent' : `${n}%`;
    out.push({ value: String(n), title });
  }
  return out;
}

/** The next zoom level one step up (1) or down (-1), kept within 50 to 200. */
export function stepZoom(current: number, dir: 1 | -1): number {
  return Math.min(200, Math.max(50, current + dir * 10));
}
