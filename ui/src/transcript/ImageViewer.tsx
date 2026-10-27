// The image viewer: one picture fitted to the window at its own aspect ratio, with its name, size
// and pixel dimensions, and Previous / Next (or the arrow keys) when there are several. The images
// attached in the composer and the images in sent messages open in this same viewer. Escape, the
// close button or a click anywhere but the picture closes it.

import { useEffect, useState, type KeyboardEvent } from 'react';
import { Dialog } from '../ui/index.ts';
import { IconChevronLeft, IconChevronRight } from '../ui/icons.tsx';
import { fitImage, imageFacts, stepImage } from './messageImages.ts';
import './messageImages.css';

export interface ViewerImage {
  key: string;
  name: string;
  /** Bytes, when known. */
  size?: number;
  /** Pixels, when known before the picture loads (0 when not). */
  width?: number;
  height?: number;
}

export interface ImageViewerProps {
  images: ViewerImage[];
  index: number;
  onIndex: (index: number) => void;
  onClose: () => void;
  /** The picture for `images[index]`, or null when there is nothing to show yet. */
  src: string | null;
  /** `loading` while a stand-in (a thumbnail) shows; `failed` when the picture cannot be had. */
  state?: 'loading' | 'ready' | 'failed';
  testId?: string;
  imageTestId?: string;
}

/** Room the dialog's own header, padding and facts line take around the picture. */
const CHROME_WIDTH = 44 + 40 + 2;
const CHROME_HEIGHT = 35 + 110;
const MAX_WIDTH = 1400;

function useWindowSize(): { width: number; height: number } {
  const [size, setSize] = useState(() => ({ width: innerWidth, height: innerHeight }));
  useEffect(() => {
    const on = () => setSize({ width: innerWidth, height: innerHeight });
    window.addEventListener('resize', on);
    return () => window.removeEventListener('resize', on);
  }, []);
  return size;
}

export function ImageViewer({ images, index, onIndex, onClose, src, state = 'ready', testId = 'image-viewer', imageTestId = 'image-viewer-image' }: ImageViewerProps) {
  const image = images[index] ?? images[0];
  const count = images.length;
  const win = useWindowSize();
  // The loaded picture's own size: it wins over the size given up front, which only holds the
  // layout steady until the picture arrives.
  const [natural, setNatural] = useState<{ key: string; width: number; height: number } | null>(null);
  if (!image) return null;
  const known = image.width && image.height ? { width: image.width, height: image.height } : null;
  const loaded = natural?.key === image.key ? natural : null;
  const dims = loaded ?? known;
  const room = { width: Math.min(MAX_WIDTH, win.width - CHROME_WIDTH), height: win.height - Math.max(70, win.height * 0.1) - CHROME_HEIGHT };
  const box = dims ? fitImage(dims.width, dims.height, room.width, room.height) : null;
  const facts = imageFacts({ size: image.size, width: dims?.width, height: dims?.height });
  const step = (delta: 1 | -1) => onIndex(stepImage(index, count, delta));
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (count < 2 || e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return;
    if (e.key === 'ArrowLeft' || e.key === 'ArrowUp') step(-1);
    else if (e.key === 'ArrowRight' || e.key === 'ArrowDown') step(1);
    else return;
    e.preventDefault();
    // The dialog is portalled, but React still bubbles its keys to whatever opened it.
    e.stopPropagation();
  };
  const name = image.name.trim() === '' ? 'Image' : image.name;
  return (
    <Dialog
      title={name}
      className="image-viewer"
      testId={testId}
      onClose={onClose}
      onKeyDown={onKeyDown}
      headerActions={
        count > 1 && (
          <>
            <span className="image-viewer-count" data-testid="image-viewer-count">
              {index + 1} of {count}
            </span>
            <button type="button" className="ui-btn ui-btn-icon" aria-label="Previous image" title="Previous image (←)" data-testid="image-viewer-prev" onClick={() => step(-1)}>
              <IconChevronLeft size={17} />
            </button>
            <button type="button" className="ui-btn ui-btn-icon" aria-label="Next image" title="Next image (→)" data-testid="image-viewer-next" onClick={() => step(1)}>
              <IconChevronRight size={17} />
            </button>
          </>
        )
      }
    >
      <div
        className="image-viewer-body"
        onMouseDown={(e) => {
          // A press anywhere but the picture itself closes, like a press on the backdrop.
          if (!(e.target instanceof HTMLImageElement)) onClose();
        }}
      >
        <div className="image-viewer-stage">
          {src && state !== 'failed' ? (
            <img
              key={image.key}
              className={`image-viewer-img${box ? '' : ' image-viewer-unsized'}${state === 'loading' ? ' image-viewer-pending' : ''}`}
              src={src}
              alt={name}
              style={box ? { width: box.width, height: box.height } : undefined}
              draggable={false}
              data-testid={imageTestId}
              data-state={state}
              onLoad={(e) => {
                if (state === 'ready') setNatural({ key: image.key, width: e.currentTarget.naturalWidth, height: e.currentTarget.naturalHeight });
              }}
            />
          ) : (
            <span className="image-viewer-missing" data-testid={imageTestId} data-state={state === 'failed' ? 'failed' : 'loading'}>
              {state === 'failed' ? 'This image could not be loaded.' : 'Loading…'}
            </span>
          )}
        </div>
        {facts && (
          <div className="image-viewer-facts" data-testid="image-viewer-facts">
            {facts}
          </div>
        )}
      </div>
    </Dialog>
  );
}
