// A gallery of pictures the agent showed (app_show_images): one large picture with its caption,
// swipe (drag or touchpad), arrow keys, dots and a strip of thumbnails. A click opens the picture
// full size. The host keeps its own copy of the bytes, so the card outlives the files it came from.

import { useEffect, useMemo, useState, type KeyboardEvent } from 'react';
import { cardOps, type ShownGallery } from '../api/ops.ts';
import { useShell } from '../shell/context.ts';
import { IconChevronLeft, IconChevronRight } from '../ui/icons.tsx';
import { stepImage } from './messageImages.ts';
import { ShownViewer } from './ShownViewer.tsx';
import { useShownImage } from './shownImages.ts';
import { useSwipe } from './swipe.ts';
import './cards.css';

type Fetched = { state: 'loading' } | { state: 'ready'; gallery: ShownGallery } | { state: 'failed' };

function useGallery(chatId: string, galleryId: string): Fetched {
  const { api } = useShell();
  const [fetched, setFetched] = useState<Fetched>({ state: 'loading' });
  useEffect(() => {
    let live = true;
    setFetched({ state: 'loading' });
    cardOps(api)
      .gallery(chatId, galleryId)
      .then((gallery) => live && setFetched({ state: 'ready', gallery }))
      .catch(() => live && setFetched({ state: 'failed' }));
    return () => {
      live = false;
    };
  }, [api, chatId, galleryId]);
  return fetched;
}

/** One picture as a small thumbnail, a button when it has a click to give. */
export function ShownThumb({ chatId, galleryId, index, label, className, onClick }: { chatId: string; galleryId: string; index: number; label: string; className: string; onClick?: () => void }) {
  const loaded = useShownImage(chatId, galleryId, index, true);
  const inner =
    loaded.state === 'ready' ? <img src={loaded.url} alt="" draggable={false} /> : <span className="gc-thumb-wait">{loaded.state === 'failed' ? '!' : ''}</span>;
  if (!onClick) return <span className={className}>{inner}</span>;
  return (
    <button type="button" className={className} aria-label={label} title={label} onClick={onClick}>
      {inner}
    </button>
  );
}

export function GalleryCard({ chatId, galleryId }: { chatId: string; galleryId: string }) {
  const fetched = useGallery(chatId, galleryId);
  if (fetched.state === 'loading') return <div className="gallery-card gallery-card-wait" data-testid="gallery-card" data-state="loading" aria-busy="true" />;
  if (fetched.state === 'failed' || fetched.gallery.images.length === 0) {
    return (
      <div className="gallery-card gallery-card-failed muted" data-testid="gallery-card" data-state="failed">
        These pictures are no longer available.
      </div>
    );
  }
  return <Gallery chatId={chatId} gallery={fetched.gallery} />;
}

function Gallery({ chatId, gallery }: { chatId: string; gallery: ShownGallery }) {
  const { images } = gallery;
  const count = images.length;
  const [index, setIndex] = useState(0);
  const [viewing, setViewing] = useState(false);
  const at = Math.min(index, count - 1);
  const current = images[at]!;
  const go = (i: number) => setIndex(Math.min(Math.max(0, i), count - 1));
  const step = (delta: 1 | -1) => setIndex(stepImage(at, count, delta));
  const swipe = useSwipe(step, count > 1);
  const full = useShownImage(chatId, gallery.galleryId, at, false);
  const thumb = useShownImage(chatId, gallery.galleryId, at, true);
  const src = full.state === 'ready' ? full.url : thumb.state === 'ready' ? thumb.url : null;
  const pictures = useMemo(() => images.map((im, i) => ({ name: im.caption || im.name || `Picture ${i + 1}`, width: im.width, height: im.height })), [images]);
  const onKeyDown = (e: KeyboardEvent<HTMLElement>) => {
    if (count < 2 || e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return;
    if (e.key === 'ArrowLeft') step(-1);
    else if (e.key === 'ArrowRight') step(1);
    else if (e.key === 'Home') go(0);
    else if (e.key === 'End') go(count - 1);
    else return;
    e.preventDefault();
  };
  const title = gallery.title?.trim() || (count === 1 ? 'Picture' : 'Pictures');
  return (
    <div className="gallery-card" data-testid="gallery-card" data-state="ready" data-gallery={gallery.galleryId} data-count={count} data-index={at}>
      <div className="gc-head">
        <span className="gc-title">{title}</span>
        {count > 1 && (
          <span className="gc-count muted" data-testid="gallery-count">
            {at + 1} of {count}
          </span>
        )}
      </div>
      <div className="gc-stage" role="group" aria-roledescription="carousel" aria-label={title} tabIndex={0} onKeyDown={onKeyDown} {...swipe}>
        {src ? (
          <button type="button" className="gc-open" title="Open full size" aria-label={`Open ${pictures[at]!.name} full size`} data-testid="gallery-open" onClick={() => setViewing(true)}>
            <img className={`gc-image${full.state === 'ready' ? '' : ' gc-image-pending'}`} src={src} alt={current.caption || current.name} draggable={false} data-testid="gallery-image" />
          </button>
        ) : (
          <span className="gc-missing muted" data-testid="gallery-image" data-state={full.state === 'failed' ? 'failed' : 'loading'}>
            {full.state === 'failed' ? 'This picture could not be loaded.' : 'Loading…'}
          </span>
        )}
        {count > 1 && (
          <>
            <button type="button" className="gc-nav gc-prev" aria-label="Previous picture" title="Previous picture (←)" data-testid="gallery-prev" onClick={() => step(-1)}>
              <IconChevronLeft size={18} />
            </button>
            <button type="button" className="gc-nav gc-next" aria-label="Next picture" title="Next picture (→)" data-testid="gallery-next" onClick={() => step(1)}>
              <IconChevronRight size={18} />
            </button>
          </>
        )}
      </div>
      {current.caption && (
        <div className="gc-caption" data-testid="gallery-caption">
          {current.caption}
        </div>
      )}
      {count > 1 && (
        <div className="gc-dots" role="tablist" aria-label="Pictures">
          {images.map((im, i) => (
            <button key={im.index} type="button" role="tab" aria-selected={i === at} aria-label={`Picture ${i + 1}`} className={`gc-dot${i === at ? ' on' : ''}`} data-testid="gallery-dot" onClick={() => go(i)} />
          ))}
        </div>
      )}
      {count > 1 && (
        <div className="gc-strip" data-testid="gallery-strip">
          {images.map((im, i) => (
            <ShownThumb key={im.index} chatId={chatId} galleryId={gallery.galleryId} index={i} label={`Show picture ${i + 1}`} className={`gc-thumb${i === at ? ' on' : ''}`} onClick={() => go(i)} />
          ))}
        </div>
      )}
      {viewing && <ShownViewer chatId={chatId} galleryId={gallery.galleryId} pictures={pictures} index={at} onIndex={setIndex} onClose={() => setViewing(false)} />}
    </div>
  );
}
