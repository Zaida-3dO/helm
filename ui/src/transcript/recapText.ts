// The recap's one-line summary, e.g. "Done in 4m 12s · 5 of 6 steps · 3 files edited · 2 pictures".

import { pictureCount, stepsLabel, type Recap } from './recap.ts';
import { formatElapsed } from './workBlocks.ts';

export function recapLine(recap: Recap): string {
  const parts = [recap.elapsedMs >= 1000 ? `Done in ${formatElapsed(recap.elapsedMs)}` : 'Done', stepsLabel(recap)];
  if (recap.files > 0) parts.push(`${recap.files} ${recap.files === 1 ? 'file' : 'files'} edited`);
  const pictures = pictureCount(recap);
  if (pictures > 0) parts.push(`${pictures} ${pictures === 1 ? 'picture' : 'pictures'}`);
  return parts.join(' · ');
}
