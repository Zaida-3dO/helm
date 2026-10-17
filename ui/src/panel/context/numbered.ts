// "Add to message": pending review comments as a numbered list for the draft. Each
// quote sits in a fence longer than any backtick run inside it, so a quote cannot close it early.

import type { ReviewComment } from '../../api/panel.ts';

const ORIGIN: Record<string, string> = { head: 'working tree', staged: 'staged change', unstaged: 'working tree' };

export function numbered(comments: ReviewComment[]): string {
  const lines = ['Please address these review comments:', ''];
  comments.forEach((c, i) => {
    const longest = Math.max(0, ...[...c.quote.matchAll(/`+/g)].map((m) => m[0].length));
    const fence = '`'.repeat(Math.max(3, longest + 1));
    lines.push(
      `${i + 1}. ${c.path} (${ORIGIN[c.diffSide] ?? c.diffSide}, ${c.side === 'left' ? 'old' : 'new'} lines ${c.startLine}–${c.endLine})`,
      fence,
      c.quote,
      fence,
      c.body,
      '',
    );
  });
  return lines.join('\n').trimEnd();
}
