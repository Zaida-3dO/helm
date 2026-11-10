// The full-size viewer over a gallery's pictures, with the thumbnail standing in while the picture
// loads. It steps through the gallery by button, arrow key or swipe (ImageViewer).

import { useMemo } from 'react';
import { ImageViewer } from './ImageViewer.tsx';
import { peekShownThumb, useShownImage } from './shownImages.ts';

export interface ShownPicture {
  name: string;
  width?: number;
  height?: number;
}

export function ShownViewer({
  chatId,
  galleryId,
  pictures,
  index,
  onIndex,
  onClose,
}: {
  chatId: string;
  galleryId: string;
  pictures: ShownPicture[];
  index: number;
  onIndex: (i: number) => void;
  onClose: () => void;
}) {
  const full = useShownImage(chatId, galleryId, index, false);
  const thumb = peekShownThumb(chatId, galleryId, index);
  const images = useMemo(() => pictures.map((p, i) => ({ key: `${galleryId}/${i}`, name: p.name, width: p.width, height: p.height })), [galleryId, pictures]);
  return (
    <ImageViewer
      images={images}
      index={index}
      onIndex={onIndex}
      onClose={onClose}
      src={full.state === 'ready' ? full.url : thumb}
      state={full.state === 'ready' ? 'ready' : full.state === 'failed' && !thumb ? 'failed' : 'loading'}
      testId="shown-image-viewer"
      imageTestId="shown-image-full"
    />
  );
}
