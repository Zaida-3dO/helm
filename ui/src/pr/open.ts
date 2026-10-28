// Opening a web address from the PR tab: always outside the app, never back into a PR tab (so
// "Open on GitHub" cannot loop), and only after a click.

import { handleLink } from '../panel/links.ts';
import { prUrl } from './links.ts';
import type { PrRef, PrView } from './types.ts';

export function handleLinkOpen(url: string): void {
  void handleLink({ kind: 'url', href: url }, { chatId: null }, { skipWebRoutes: true });
}

export function openOnGitHub(ref: PrRef, view: PrView): void {
  handleLinkOpen(prUrl(ref, view));
}
