// Markdown wrapping for the editor's formatting keys. Pure.

/** Wrap `selected` in `marker`. With nothing selected the caret goes between the markers. */
export function wrapMarker(selected: string, marker: string): { text: string; caretBack: number } {
  if (!selected) return { text: marker + marker, caretBack: marker.length };
  // Already wrapped: unwrap.
  if (selected.length >= marker.length * 2 && selected.startsWith(marker) && selected.endsWith(marker)) {
    return { text: selected.slice(marker.length, selected.length - marker.length), caretBack: 0 };
  }
  return { text: marker + selected + marker, caretBack: 0 };
}
