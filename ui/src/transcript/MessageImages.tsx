// The images the user sent with a message, as thumbnails in their bubble. The row carries only each
// image's id, name and size (details.images). The host answers a small cached thumbnail when the row
// is on screen (freed when it leaves); the full image is fetched only when a click opens it in the
// image viewer, which steps through the message's other images too.

import { useEffect, useMemo, useState } from 'react';
import type { MessageImage } from '../api/generated/MessageImage.ts';
import { useShell } from '../shell/context.ts';
import { ImageViewer } from './ImageViewer.tsx';
import { UrlCache, bytesFromBase64, imageLabel, thumbSize } from './messageImages.ts';
import './messageImages.css';

type Loaded = { state: 'loading' } | { state: 'ready'; url: string } | { state: 'failed' };

/** Thumbnails kept after their row scrolls away, so scrolling back does not ask the host again. */
const thumbUrls = new UrlCache(64, (url) => URL.revokeObjectURL(url));

function useImageUrl(chatId: string, id: string, thumb: boolean): Loaded {
  const { api } = useShell();
  const key = `${chatId}/${id}`;
  // A kept thumbnail shows on the first render; the effect below takes its use.
  const [loaded, setLoaded] = useState<Loaded>(() => {
    const kept = thumb ? thumbUrls.peek(key) : undefined;
    return kept ? { state: 'ready', url: kept } : { state: 'loading' };
  });
  useEffect(() => {
    let url: string | null = null;
    let cancelled = false;
    // A thumbnail is shared and kept; the full image belongs to the dialog that opened it.
    const kept = thumb ? thumbUrls.acquire(key) : undefined;
    if (kept) {
      setLoaded({ state: 'ready', url: kept });
    } else {
      setLoaded({ state: 'loading' });
      api<{ mime: string; bytes: string }>('attachment_get', { chatId, attachmentId: id, thumb })
        .then((r) => {
          if (cancelled) return;
          url = URL.createObjectURL(new Blob([bytesFromBase64(r.bytes)], { type: r.mime }));
          if (thumb) {
            // Another row may have fetched the same image meanwhile: share its URL, drop ours.
            const shared = thumbUrls.put(key, url);
            if (shared !== url) URL.revokeObjectURL(url);
            url = shared;
          }
          setLoaded({ state: 'ready', url });
        })
        .catch(() => {
          if (!cancelled) setLoaded({ state: 'failed' });
        });
    }
    return () => {
      cancelled = true;
      if (thumb) {
        if (kept || url) thumbUrls.release(key);
      } else if (url) {
        URL.revokeObjectURL(url);
      }
    };
  }, [api, chatId, id, key, thumb]);
  return loaded;
}

/** The viewer over a message's images: the full-size picture, with its thumbnail standing in while it loads. */
function MessageImageViewer({ chatId, images, index, onIndex, onClose }: { chatId: string; images: MessageImage[]; index: number; onIndex: (i: number) => void; onClose: () => void }) {
  const image = images[index]!;
  const full = useImageUrl(chatId, image.id, false);
  const thumb = thumbUrls.peek(`${chatId}/${image.id}`) ?? null;
  const viewerImages = useMemo(() => images.map((im) => ({ key: im.id, name: im.name, width: im.width, height: im.height })), [images]);
  return (
    <ImageViewer
      images={viewerImages}
      index={index}
      onIndex={onIndex}
      onClose={onClose}
      src={full.state === 'ready' ? full.url : thumb}
      state={full.state === 'ready' ? 'ready' : full.state === 'failed' && !thumb ? 'failed' : 'loading'}
      testId="message-image-viewer"
      imageTestId="message-image-full"
    />
  );
}

function Thumb({ chatId, image, label, onOpen }: { chatId: string; image: MessageImage; label: string; onOpen: () => void }) {
  const loaded = useImageUrl(chatId, image.id, true);
  const size = thumbSize(image.width, image.height);
  const box = { width: size.width, height: size.height };
  if (loaded.state === 'failed') {
    return (
      <span className="msg-image msg-image-failed" style={box} role="img" aria-label={`${label} could not be loaded`} data-testid="message-image" data-state="failed">
        <span>Image unavailable</span>
      </span>
    );
  }
  if (loaded.state === 'loading') {
    return <span className="msg-image msg-image-loading" style={box} aria-busy="true" aria-label={`Loading ${label}`} data-testid="message-image" data-state="loading" />;
  }
  return (
    <button type="button" className="msg-image" style={box} title={`${label}: open full size`} aria-label={`Open ${label} full size`} data-testid="message-image" data-state="ready" onClick={onOpen}>
      <img src={loaded.url} alt={label} width={size.width} height={size.height} draggable={false} />
    </button>
  );
}

/** Thumbnails for a user row's images; nothing when it has none. */
export function MessageImages({ chatId, images }: { chatId: string; images: MessageImage[] | undefined }) {
  const [open, setOpen] = useState<number | null>(null);
  if (!images || images.length === 0) return null;
  return (
    <div className="msg-images" data-testid="message-images">
      {images.map((image, i) => (
        <Thumb key={image.id} chatId={chatId} image={image} label={imageLabel(image.name, i, images.length)} onOpen={() => setOpen(i)} />
      ))}
      {open !== null && images[open] && <MessageImageViewer chatId={chatId} images={images} index={open} onIndex={setOpen} onClose={() => setOpen(null)} />}
    </div>
  );
}
