// The images the user sent with a message, as thumbnails in their bubble. The row carries only each
// image's id, name and size (details.images); the bytes come from `attachment_get` when the row is
// on screen and are freed when it leaves. A click opens the full image in a dialog.

import { useEffect, useState } from 'react';
import type { MessageImage } from '../api/generated/MessageImage.ts';
import { useShell } from '../shell/context.ts';
import { Dialog } from '../ui/index.ts';
import { bytesFromBase64, imageLabel, thumbSize } from './messageImages.ts';
import './messageImages.css';

type Loaded = { state: 'loading' } | { state: 'ready'; url: string } | { state: 'failed' };

function useImageUrl(chatId: string, id: string): Loaded {
  const { api } = useShell();
  const [loaded, setLoaded] = useState<Loaded>({ state: 'loading' });
  useEffect(() => {
    let url: string | null = null;
    let cancelled = false;
    setLoaded({ state: 'loading' });
    api<{ mime: string; bytes: string }>('attachment_get', { chatId, attachmentId: id })
      .then((r) => {
        if (cancelled) return;
        url = URL.createObjectURL(new Blob([bytesFromBase64(r.bytes)], { type: r.mime }));
        setLoaded({ state: 'ready', url });
      })
      .catch(() => {
        if (!cancelled) setLoaded({ state: 'failed' });
      });
    return () => {
      cancelled = true;
      if (url) URL.revokeObjectURL(url);
    };
  }, [api, chatId, id]);
  return loaded;
}

function Thumb({ chatId, image, label }: { chatId: string; image: MessageImage; label: string }) {
  const loaded = useImageUrl(chatId, image.id);
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
          <img className="msg-image-full" src={loaded.url} alt={label} />
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
