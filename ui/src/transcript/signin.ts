// Whether a failed turn's message means Claude's own sign-in has expired or is missing, so the
// row can offer to sign in again. Pure, so the list of texts is tested as a table.

/** Lower-case fragments that, on their own, mean the sign-in is the problem. */
const SIGN_IN_TEXTS = [
  'failed to authenticate',
  'oauth session expired',
  'oauth token has expired',
  'oauth access token has expired',
  'not logged in',
  'please run /login',
  'authentication_error',
  'authentication_failed',
];

export function isSignInError(text: string | null | undefined): boolean {
  if (!text) return false;
  const t = text.toLowerCase();
  if (SIGN_IN_TEXTS.some((s) => t.includes(s))) return true;
  return t.includes('invalid api key') && t.includes('/login');
}
