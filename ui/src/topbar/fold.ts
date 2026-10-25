// When the top bar's action buttons fold into one "Actions" menu. The breadcrumb is the bar's only flexible part (flex 1, basis 0), so its width is the
// bar's free space; when the bar's content overflows, the overflow is negative free space. The
// row folds once the breadcrumb would get less than MIN_TITLE_ROOM, and unfolds only when the
// inline buttons would fit again with a little to spare, so a resize near the edge does not flap.

/** Pixels the breadcrumb (project › title) keeps before the action buttons fold. */
export const MIN_TITLE_ROOM = 150;
/** Extra room needed to unfold, so the row does not flip back and forth at the threshold. */
export const UNFOLD_SLACK = 12;

/** How much of the bar fits, from its own width (the main pane's width): `wide` shows everything;
 *  `mid` drops the usage bars' tracks (the percentages stay); `narrow` moves the meter, Fork, Pin and
 *  Archive out (the chat menu still has them); `tight` also hides the action row, so the title, the
 *  panel and menu buttons and the window buttons always fit. */
export type TopBarTier = 'wide' | 'mid' | 'narrow' | 'tight';

export function topBarTier(width: number): TopBarTier {
  return width >= 900 ? 'wide' : width >= 640 ? 'mid' : width >= 440 ? 'narrow' : 'tight';
}

/** The breadcrumb's real room: its width less however far the bar's content overflows. */
export function titleRoom(crumbsWidth: number, barScrollWidth: number, barClientWidth: number): number {
  return crumbsWidth - Math.max(0, barScrollWidth - barClientWidth);
}

/**
 * Whether the row should be folded next. `inlineWidth` is the width the unfolded row last took;
 * `foldedWidth` the folded trigger's. Unknown widths (0) never unfold a folded row.
 */
export function shouldFold(folded: boolean, room: number, inlineWidth: number, foldedWidth: number, min = MIN_TITLE_ROOM): boolean {
  if (!folded) return room < min;
  if (inlineWidth <= 0) return true;
  return room - (inlineWidth - foldedWidth) < min + UNFOLD_SLACK;
}
