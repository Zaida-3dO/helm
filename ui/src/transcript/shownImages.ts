// The pictures an agent put in the chat (galleries and option boards): thumbnails are kept after
// their card scrolls away, full-size pictures are fetched for the one on screen.

import { useMemo } from 'react';
import { cardOps } from '../api/ops.ts';
import { useShell } from '../shell/context.ts';
import { UrlCache } from './messageImages.ts';
import { useBlobUrl, revokeUrl, type Loaded } from './useBlobUrl.ts';

const thumbUrls = new UrlCache(96, revokeUrl);
const fullUrls = new UrlCache(8, revokeUrl);

/** The cached thumbnail's URL, if it has been fetched already (a viewer uses it while the full picture loads). */
export function peekShownThumb(chatId: string, galleryId: string, index: number): string | null {
  return thumbUrls.peek(`${chatId}/${galleryId}/${index}`) ?? null;
}

/** One picture of a gallery (or one option's picture, for a board id), as a thumbnail or full size. */
export function useShownImage(chatId: string, galleryId: string, index: number, thumb: boolean): Loaded {
  const { api } = useShell();
  const ops = useMemo(() => cardOps(api), [api]);
  return useBlobUrl(
    `${chatId}/${galleryId}/${index}${thumb ? '' : '/full'}`,
    thumb ? thumbUrls : fullUrls,
    () => (thumb ? ops.thumb(chatId, galleryId, index) : ops.image(chatId, galleryId, index)),
  );
}
