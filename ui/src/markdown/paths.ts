// Lexical path safety, shared by the Markdown classifier and the panel's link gate.
//
// On Windows, merely opening or canonicalising a UNC path (\\host\share, //host/share, or a
// file://host/ URL) makes the OS contact that SMB server and offer the user's NTLM credentials.
// Device namespaces (\\?\, \\.\, \??\) bypass normal path parsing. Both are refused here, from the
// string alone, before anything could touch the filesystem. The host repeats the same check in Rust.

// eslint-disable-next-line no-control-regex
const CONTROL = /[\u0000-\u001f\u007f]/g;

function safeDecode(value: string): string {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}

/** The path part of a `file:` URL, or the string itself; slashes normalised to `/`. */
function pathPart(form: string): { authority: string | null; path: string } {
  const n = form.replace(/\\/g, '/');
  if (!/^file:/i.test(n)) return { authority: null, path: n };
  const rest = n.slice(5);
  if (!rest.startsWith('//')) return { authority: null, path: rest };
  const afterSlashes = rest.slice(2);
  const end = afterSlashes.indexOf('/');
  const authority = end < 0 ? afterSlashes : afterSlashes.slice(0, end);
  return { authority, path: end < 0 ? '' : afterSlashes.slice(end) };
}

function remoteOrDevice(form: string): boolean {
  const { authority, path } = pathPart(form);
  // `file://C:/x` is a common misspelling of `file:///C:/x`; a lone drive letter is not a host.
  const local = authority === null || authority === '' || authority.toLowerCase() === 'localhost' || /^[a-z]:$/i.test(authority);
  if (!local) return true;
  // Two leading slashes of either kind: UNC (\\host), device (\\?\, \\.\) or a file:////host URL.
  if (/^\/\//.test(path)) return true;
  // The NT object-manager prefix.
  if (/^\/\?\?\//.test(path)) return true;
  return false;
}

/**
 * True when a path or `file:` URL names a network share or a Windows device path, in its written
 * or its percent-decoded form. Such a target must never reach a filesystem call.
 */
export function isRemoteOrDevicePath(raw: string): boolean {
  const s = raw.replace(CONTROL, '').trim();
  if (!s) return false;
  return remoteOrDevice(s) || remoteOrDevice(safeDecode(s));
}

/**
 * How a string starts with a drive letter: `absolute` for `C:\` or `C:/`, `relative` for the
 * drive-relative `C:x` (which also covers `C:javascript:...`), or null when it has no drive.
 */
export function driveForm(s: string): 'absolute' | 'relative' | null {
  if (!/^[a-z]:/i.test(s)) return null;
  const next = s.charAt(2);
  return next === '\\' || next === '/' ? 'absolute' : 'relative';
}
