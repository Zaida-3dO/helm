// The one place a Lucide glyph becomes a kit icon. Every icon in `icons.tsx` goes through
// `kitIcon`, which fixes the kit's defaults: 14 px, stroke 2, `currentColor`, never shrinking in a
// flex row (`.ui-icon`), and hidden from screen readers unless it is given a label of its own.
//
// Written with createElement rather than JSX so `node --test` can load it (icons.test.ts).

import { createElement, type ComponentType, type SVGProps } from 'react';

/** Props every kit icon takes. Anything else goes straight to the `<svg>`. */
export interface IconProps extends Omit<SVGProps<SVGSVGElement>, 'ref' | 'width' | 'height'> {
  /** Pixel size of the square box. Default 14. */
  size?: number;
  /** Stroke width. Default 2; use 2.2–2.6 for tiny glyphs and 1.6–1.8 for large ones. */
  strokeWidth?: number;
  /** Fill the glyph (a pinned pin, the stop square). */
  filled?: boolean;
  /** Give the icon its own accessible name. Without it the icon is decorative (aria-hidden). */
  label?: string;
}

export const ICON_DEFAULT_SIZE = 14;
export const ICON_DEFAULT_STROKE = 2;

/** The shape of a Lucide component, kept loose so this file does not depend on Lucide's types. */
type Glyph = ComponentType<Record<string, unknown>>;

export function kitIcon(glyph: Glyph, name: string): ComponentType<IconProps> {
  function KitIcon({ size = ICON_DEFAULT_SIZE, strokeWidth = ICON_DEFAULT_STROKE, filled, label, className, ...rest }: IconProps) {
    return createElement(glyph, {
      ...rest,
      size,
      strokeWidth,
      fill: filled ? 'currentColor' : 'none',
      className: className ? `ui-icon ${className}` : 'ui-icon',
      'data-icon': name,
      ...(label ? { role: 'img', 'aria-label': label, 'aria-hidden': undefined } : { 'aria-hidden': true, focusable: 'false' }),
    });
  }
  KitIcon.displayName = `Icon(${name})`;
  return KitIcon;
}
