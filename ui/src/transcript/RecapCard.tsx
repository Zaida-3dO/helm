// The recap under a settled turn's final reply: how long it took, the steps done, the files edited
// and the pictures shown, all worked out from the turn's rows (recap.ts). The checklist's last
// state sits behind a toggle with its unfinished steps marked; the edited-files card comes from the
// side panel through a recap section; the turn's galleries show as thumbnails that open full size.

import { useMemo, useState } from 'react';
import type { MessageRow } from '../api/types.ts';
import { IconCheck } from '../ui/icons.tsx';
import { Glyph } from './icons.tsx';
import { ShownThumb } from './GalleryCard.tsx';
import { ChecklistSteps } from './LiveChecklist.tsx';
import { pictureCount, type Recap } from './recap.ts';
import { recapLine } from './recapText.ts';
import { ShownViewer } from './ShownViewer.tsx';
import { recapSectionsFor, type SlotContext } from './slots.ts';
import './cards.css';

/** Thumbnails shown across a turn's galleries before "+N more". */
const MAX_THUMBS = 8;

interface Viewing {
  galleryId: string;
  count: number;
  index: number;
}

export function RecapCard({ recap, row, ctx }: { recap: Recap; row: MessageRow; ctx: SlotContext }) {
  const [steps, setSteps] = useState(false);
  const [viewing, setViewing] = useState<Viewing | null>(null);
  const thumbs = useMemo(() => {
    const all = recap.galleries.flatMap((g) => Array.from({ length: g.count }, (_, index) => ({ galleryId: g.galleryId, count: g.count, index })));
    return { shown: all.slice(0, MAX_THUMBS), more: Math.max(0, all.length - MAX_THUMBS) };
  }, [recap.galleries]);
  const pictures = useMemo(() => (viewing ? Array.from({ length: viewing.count }, (_, i) => ({ name: `Picture ${i + 1}` })) : []), [viewing?.galleryId, viewing?.count]);
  const sections = recapSectionsFor();
  const checklist = recap.checklist;
  return (
    <div className="recap-card" data-testid="recap-card" data-steps={checklist?.length ?? recap.toolSteps} data-files={recap.files} data-pictures={pictureCount(recap)}>
      <div className="rc-head">
        <span className="rc-tick" aria-hidden="true">
          <IconCheck size={13} />
        </span>
        <span className="rc-line" data-testid="recap-line">
          {recapLine(recap)}
        </span>
        {checklist && (
          <button type="button" className="rc-toggle" aria-expanded={steps} data-testid="recap-steps-toggle" onClick={() => setSteps((o) => !o)}>
            <Glyph name="chevron" size={11} className="rc-chevron" />
            Steps
          </button>
        )}
      </div>
      {steps && checklist && <ChecklistSteps todos={checklist} final />}
      {thumbs.shown.length > 0 && (
        <div className="rc-thumbs" data-testid="recap-pictures">
          {thumbs.shown.map((t) => (
            <ShownThumb
              key={`${t.galleryId}/${t.index}`}
              chatId={ctx.chatId}
              galleryId={t.galleryId}
              index={t.index}
              label={`Open picture ${t.index + 1} full size`}
              className="gc-thumb rc-thumb"
              onClick={() => setViewing(t)}
            />
          ))}
          {thumbs.more > 0 && <span className="rc-more muted">+{thumbs.more}</span>}
        </div>
      )}
      {sections.map((s) => (
        <div key={s.id} className="rc-section" data-recap-section={s.id}>
          {s.render(row, ctx)}
        </div>
      ))}
      {viewing && <ShownViewer chatId={ctx.chatId} galleryId={viewing.galleryId} pictures={pictures} index={viewing.index} onIndex={(index) => setViewing({ ...viewing, index })} onClose={() => setViewing(null)} />}
    </div>
  );
}
