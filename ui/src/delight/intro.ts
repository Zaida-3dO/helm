// The first-run intro's rules, kept apart from the screen so they can be tested: when it has been
// seen, how long it lasts, and when to wait before showing it.

/** The whole intro, from the scrim appearing to it gone. */
export const INTRO_MS = 2800;
/** The still card shown when motion is off: it fades in and out over this long. */
export const INTRO_STILL_MS = 1200;
/** When, in the moving intro, the wheel has settled (the chime plays then). */
export const INTRO_SETTLE_MS = 1600;

const PREFIX = 'helm.introSeen.';

/** The storage key for a profile (`default` when none is chosen). */
export function introKey(profile: string): string {
  return `${PREFIX}${profile || 'default'}`;
}

type Reader = Pick<Storage, 'getItem'> | null;
type Writer = Pick<Storage, 'setItem'> | null;

/** Whether the intro was shown before. Unreadable storage counts as seen, so it can never loop. */
export function introSeen(store: Reader, key: string): boolean {
  try {
    return !store || store.getItem(key) === '1';
  } catch {
    return true;
  }
}

export function markIntroSeen(store: Writer, key: string): void {
  try {
    store?.setItem(key, '1');
  } catch {
    // Nowhere to remember it: introSeen treats unwritable storage the same way next time.
  }
}

/**
 * Run `show` once the page is visible: at once when it is, else the first time it becomes so (a
 * window that starts in the tray should not spend the intro unseen). Returns the cancel function.
 */
export function whenVisible(doc: Pick<Document, 'visibilityState' | 'addEventListener' | 'removeEventListener'>, show: () => void): () => void {
  if (doc.visibilityState === 'visible') {
    show();
    return () => undefined;
  }
  const on = () => {
    if (doc.visibilityState !== 'visible') return;
    doc.removeEventListener('visibilitychange', on);
    show();
  };
  doc.addEventListener('visibilitychange', on);
  return () => doc.removeEventListener('visibilitychange', on);
}
