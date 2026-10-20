// The images the user sent with a message, as thumbnails in their bubble. The row carries only each
// image's id, name and size (details.images). The host answers a small cached thumbnail when the row
// is on screen (freed when it leaves); the full image is fetched only when a click opens it in a
// dialog.

import { useEffect, useState } from 'react';
import type { MessageImage } from '../api/generated/MessageImage.ts';
import { useShell } from '../shell/context.ts';
import { Dialog } from '../ui/index.ts';
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

/** The full-size image in the dialog; the thumbnail stands in while it loads or if it cannot load. */
function FullImage({ chatId, image, label, thumbUrl }: { chatId: string; image: MessageImage; label: string; thumbUrl: string }) {
  const full = useImageUrl(chatId, image.id, false);
  return (
    <img
      className={full.state === 'ready' ? 'msg-image-full' : 'msg-image-full msg-image-full-pending'}
      src={full.state === 'ready' ? full.url : thumbUrl}
      alt={label}
      data-testid="message-image-full"
      data-state={full.state}
    />
  );
}

function Thumb({ chatId, image, label }: { chatId: string; image: MessageImage; label: string }) {
  const loaded = useImageUrl(chatId, image.id, true);
  const [open, setOpen] = useState(false);
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
    <>
      <button type="button" className="msg-image" style={box} title={`${label}: open full size`} aria-label={`Open ${label} full size`} data-testid="message-image" data-state="ready" onClick={() => setOpen(true)}>
        <img src={loaded.url} alt={label} width={size.width} height={size.height} draggable={false} />
      </button>
      {open && (
        <Dialog title={image.name.trim() === '' ? 'Image' : image.name} wide onClose={() => setOpen(false)} testId="message-image-viewer">
          <FullImage chatId={chatId} image={image} label={label} thumbUrl={loaded.url} />
        </Dialog>
      )}
    </>
  );
}

/** Thumbnails for a user row's images; nothing when it has none. */
export function MessageImages({ chatId, images }: { chatId: string; images: MessageImage[] | undefined }) {
  if (!images || images.length === 0) return null;
  return (
    <div className="msg-images" data-testid="message-images">
      {images.map((image, i) => (
        <Thumb key={image.id} chatId={chatId} image={image} label={imageLabel(image.name, i, images.length)} />
      ))}
    </div>
  );
}
