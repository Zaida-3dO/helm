// Settings → Hub's Telegram and Voice sections show one profile at a time. These pick which one
// the pane starts on and say what the link row knows about that profile's Telegram account.

import type { ProfileSummary } from '../../api/profiles.ts';
import type { TelegramLink } from '../../api/hub.ts';

export interface ProfileChoice {
  value: string;
  title: string;
  subtitle?: string;
}

/** The profile the pane opens on: the one the sidebar shows when it names one and still exists,
 *  else the home profile, else the first. `null` only when there is no profile at all. */
export function defaultProfileId(profiles: readonly ProfileSummary[], activeId: string | null): string | null {
  if (activeId && profiles.some((p) => p.id === activeId)) return activeId;
  return profiles.find((p) => p.isHome)?.id ?? profiles[0]?.id ?? null;
}

/** Picker options: home first, then the rest by name. A profile not linked to a person on the
 *  hub says so, since its Telegram and voice rows can only work once it is. */
export function profileChoices(profiles: readonly ProfileSummary[]): ProfileChoice[] {
  const sorted = [...profiles].sort((a, b) => {
    if (a.isHome !== b.isHome) return a.isHome ? -1 : 1;
    return a.name.localeCompare(b.name, undefined, { sensitivity: 'base' });
  });
  return sorted.map((p) => ({
    value: p.id,
    title: p.name,
    subtitle: p.isHome ? 'This PC’s home profile' : p.hubPersonId ? undefined : 'Not linked to a person on the hub yet',
  }));
}

/** What the link row says about a profile's Telegram account on the hub. */
export function linkLine(link: TelegramLink | null): string {
  if (!link) return 'Checking…';
  if (!link.linked) return 'Not linked';
  return link.handle ? `Linked as ${link.handle}` : 'Linked';
}

/** Whether the link/unlink controls can be offered for a profile: it needs a person on the hub. */
export function linkable(profile: ProfileSummary | undefined): boolean {
  return !!profile?.hubPersonId;
}

/** What a prompt box shows about its text: whether Save and Reset apply. A blank box means "use
 *  the default", so it counts as a change only while a replacement is saved. */
export function promptState(
  draft: string,
  p: { text: string; default: string; custom: boolean },
  max: number,
): { dirty: boolean; tooLong: boolean; canReset: boolean } {
  const d = draft.trim();
  return { dirty: d !== p.text.trim() && !(d === '' && !p.custom), tooLong: d.length > max, canReset: p.custom };
}
