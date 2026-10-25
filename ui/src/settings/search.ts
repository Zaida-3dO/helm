// Settings search. Every pane stays mounted while the search box has text; this file decides
// which rows (and which panes) stay on screen and marks the matched words. It works on the
// rendered rows, so a pane needs no search code of its own: any `.setting-row` is searchable by
// its title, description and control text.

/** The words of a query: lower-cased, split on spaces. */
export function searchTerms(query: string): string[] {
  return query.toLowerCase().split(/\s+/).filter(Boolean);
}

/** Whether every word of the query appears in `text` (case-insensitive). An empty query matches. */
export function matchesQuery(text: string, query: string): boolean {
  const lower = text.toLowerCase();
  return searchTerms(query).every((t) => lower.includes(t));
}

const HIDE_ATTR = 'data-search-hide';
const HIGHLIGHT = 'settings-hit';

/** Hide `el`'s children that hold no kept row, descending into those that do. */
function prune(el: Element, keep: Set<Element>): void {
  for (const child of Array.from(el.children)) {
    if (keep.has(child) || child.classList.contains('settings-pane-label')) continue;
    const holdsKept = [...keep].some((k) => child.contains(k));
    if (holdsKept) prune(child, keep);
    else child.setAttribute(HIDE_ATTR, '');
  }
}

/**
 * Show only what matches `query` inside `root` (the body holding one `section[data-pane]` per
 * pane, each carrying its title in `data-title`). A pane whose title matches shows whole; any
 * other pane shows just its matching rows. Returns how many panes remain visible.
 */
export function applySearch(root: HTMLElement, query: string): number {
  for (const el of Array.from(root.querySelectorAll(`[${HIDE_ATTR}]`))) el.removeAttribute(HIDE_ATTR);
  let visible = 0;
  const marked: Element[] = [];
  for (const section of Array.from(root.querySelectorAll<HTMLElement>('section[data-pane]'))) {
    if (matchesQuery(section.dataset.title ?? '', query)) {
      visible++;
      marked.push(section);
      continue;
    }
    const rows = Array.from(section.querySelectorAll('.setting-row'));
    const keep = new Set(rows.filter((r) => matchesQuery(r.textContent ?? '', query)));
    if (keep.size === 0) {
      section.setAttribute(HIDE_ATTR, '');
      continue;
    }
    visible++;
    prune(section, keep);
    marked.push(...keep);
  }
  highlight(marked, searchTerms(query));
  return visible;
}

/** Mark each matched word in the titles and descriptions of the kept rows (CSS highlight, no DOM edits). */
function highlight(scopes: Element[], terms: string[]): void {
  const reg = (globalThis as { CSS?: { highlights?: Map<string, unknown> } }).CSS?.highlights;
  const HighlightCtor = (globalThis as { Highlight?: new (...r: Range[]) => unknown }).Highlight;
  if (!reg || !HighlightCtor) return;
  const ranges: Range[] = [];
  if (terms.length > 0) {
    for (const scope of scopes) {
      for (const text of Array.from(scope.querySelectorAll('.setting-title, .setting-desc'))) {
        const walker = document.createTreeWalker(text, NodeFilter.SHOW_TEXT);
        for (let n = walker.nextNode(); n; n = walker.nextNode()) {
          const lower = (n.nodeValue ?? '').toLowerCase();
          for (const t of terms) {
            for (let at = lower.indexOf(t); at !== -1; at = lower.indexOf(t, at + t.length)) {
              const r = document.createRange();
              r.setStart(n, at);
              r.setEnd(n, at + t.length);
              ranges.push(r);
            }
          }
        }
      }
    }
  }
  if (ranges.length === 0) reg.delete(HIGHLIGHT);
  else reg.set(HIGHLIGHT, new HighlightCtor(...ranges));
}

/** Remove the match marks (the search box was cleared or the dialog closed). */
export function clearHighlight(): void {
  (globalThis as { CSS?: { highlights?: Map<string, unknown> } }).CSS?.highlights?.delete(HIGHLIGHT);
}
