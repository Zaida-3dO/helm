// The app-wide right-click policy. Helm draws its own menus where it has them (they call
// preventDefault). Everywhere else the browser's menu (Back, Reload, Save as, Inspect...) is
// suppressed, because those entries would navigate away from or reload the app. The browser's menu
// stays where it helps: in text fields and editors (cut, copy, paste, spelling) and over selected
// text (copy). A development run keeps every native menu, for Inspect.

/** Whether a right-click at `target` should keep the browser's own menu. */
export function nativeMenuWanted(target: Element | null, selectedText: string, dev: boolean): boolean {
  if (dev) return true;
  if (selectedText.length > 0) return true;
  return !!target?.closest('input:not([type="button"]):not([type="submit"]):not([type="checkbox"]):not([type="radio"]), textarea, [contenteditable=""], [contenteditable="true"]');
}

export function installContextMenuPolicy(doc: Document, dev: boolean): () => void {
  const onMenu = (e: MouseEvent) => {
    if (e.defaultPrevented) return;
    const t = e.target as Partial<Element> | null;
    const target = typeof t?.closest === 'function' ? (t as Element) : null;
    if (!nativeMenuWanted(target, doc.getSelection()?.toString() ?? '', dev)) e.preventDefault();
  };
  doc.addEventListener('contextmenu', onMenu);
  return () => doc.removeEventListener('contextmenu', onMenu);
}
