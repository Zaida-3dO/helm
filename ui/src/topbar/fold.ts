// When the top bar's action buttons fold into one "Actions" menu. The breadcrumb is the bar's only flexible part (flex 1, basis 0), so its width is the
// bar's free space; when the bar's content overflows, the overflow is negative free space. Only
// the title gives way inside it (the project pill has its own max width and ellipsises first); its
// fixed parts (the project pill, the muted mark, the local-model chip) are not room for the title,
// so they are taken off. The row folds once the title would get less than MIN_TITLE_ROOM, and
// unfolds only when the inline buttons would fit again with a little to spare, so a resize near
// the edge does not flap.

/** Pixels the project name and title keep before the action buttons fold. */
export const MIN_TITLE_ROOM = 120;
/** Extra room needed to unfold, so the row does not flip back and forth at the threshold. */
export const UNFOLD_SLACK = 12;

/** How much of the bar fits, from its own width (the main pane's width): `wide` shows everything;
 *  `mid` drops the usage bars' tracks (the percentages stay); `narrow` moves the meter out (it is
 *  in Settings too); `tight` also hides the action row, so the title, the panel button and the
 *  window buttons always fit. */
export type TopBarTier = 'wide' | 'mid' | 'narrow' | 'tight';

export function topBarTier(width: number): TopBarTier {
  return width >= 900 ? 'wide' : width >= 640 ? 'mid' : width >= 440 ? 'narrow' : 'tight';
}

/** A tier that also sheds the optional parts: used when the title would be left too little room
 *  at the bar's own tier (a window just narrow enough to hide the sidebar, a busy local chat). */
export function shedTier(tier: TopBarTier): TopBarTier {
  return tier === 'wide' || tier === 'mid' ? 'narrow' : tier;
}

/** The title's real room: the breadcrumb's width less its fixed parts (`fixedWidth`, gaps included)
 *  and however far the bar's content overflows. */
export function titleRoom(crumbsWidth: number, fixedWidth: number, barScrollWidth: number, barClientWidth: number): number {
  return crumbsWidth - fixedWidth - Math.max(0, barScrollWidth - barClientWidth);
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

/** The breadcrumb's parts that never give way (the project pill has its own cap and ellipsises
 *  first), each with the gap after it. */
export function crumbsFixedWidth(crumbs: HTMLElement): number {
  const gap = parseFloat(getComputedStyle(crumbs).columnGap) || 0;
  let sum = 0;
  for (const el of crumbs.querySelectorAll<HTMLElement>(':scope > .crumb-muted, :scope > .crumb-status, :scope > .project-pill')) {
    sum += el.getBoundingClientRect().width + (parseFloat(getComputedStyle(el).marginLeft) || 0) + gap;
  }
  return sum;
}

/** The width the bar's optional parts (the usage meter) take now, gaps included. */
export function optionalWidth(bar: HTMLElement): number {
  const actions = bar.querySelector<HTMLElement>('.topbar-actions');
  if (!actions) return 0;
  const gap = parseFloat(getComputedStyle(actions).columnGap) || 0;
  let sum = 0;
  for (const el of actions.querySelectorAll<HTMLElement>(':scope > .usage-meter')) {
    const w = el.getBoundingClientRect().width;
    if (w > 0) sum += w + gap;
  }
  return sum;
}
