// Which reply a side chat hangs from: the last selected row when it is an
// assistant reply; a selection in a user message anchors to that turn's reply (the next assistant
// row). None means the Side chat button is not offered.

export function sideChatAnchor(rows: readonly { id: number; role: string; status: string }[], selected: readonly number[]): number | null {
  if (selected.length === 0) return null;
  const last = Math.max(...selected);
  const i = rows.findIndex((r) => r.id === last);
  if (i < 0) return null;
  for (let j = i; j < rows.length; j++) {
    const r = rows[j]!;
    if (j > i && r.role === 'user') break;
    if (r.role === 'assistant' && r.status === 'complete') return r.id;
  }
  return null;
}
