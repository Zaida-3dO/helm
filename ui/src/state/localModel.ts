// One wording for what a local model chat's work looks like from outside, everywhere it shows
// (the chat header, the sidebar, the activity dock). A local model server answers one request at
// a time and keeps the rest waiting, so a chat's turn can sit queued behind other work while its
// own agent waits in silence.

/** The chat's turn is queued at its model server. `ahead` counts the chats Helm knows would be
 * answered first; 0 means "waiting, but the count is not known". */
export function localWaitLabel(ahead: number): string {
  return ahead > 0 ? `Waiting for the local model (${ahead} ahead)` : 'Waiting for the local model';
}

/** The model server is working on this chat now. */
export const LOCAL_WORKING_LABEL = 'Using the local model';

/** A model id is a local one when it starts with the local prefix, then server, then model. */
export function isLocalModelId(id: string | null | undefined): boolean {
  return !!id && id.startsWith('local/');
}

// A local model's own name as a person would call it, when nobody named it: the family, the
// version and the size, without the quantisation, "instruct" or tags. The same reading the host
// uses for its default names, so the app never ships a raw id like `qwen3.8-flash-next-iq3_s`.
const NOISE = ['instruct', 'chat', 'latest', 'gguf', 'it', 'text'];

/** A quantisation or file-format token: `q4_k_m`, `iq3_s`, `fp16`, `bf16`, `int8`, `awq`... */
function isQuant(t: string): boolean {
  const head = t.split('_')[0] ?? t;
  const digitsAfter = (p: string) =>
    head.startsWith(p) && head.length > p.length && [...head.slice(p.length)].every((c) => c >= '0' && c <= '9');
  return ['q', 'iq', 'fp', 'bf', 'f', 'int'].some(digitsAfter) || ['awq', 'gptq', 'gguf', 'mlx'].includes(head);
}

/** `7b`, `32b`, `0.6b`, `270m`: a parameter count. */
function sizeOf(t: string): string | null {
  const unit = t.slice(-1);
  if ((unit !== 'b' && unit !== 'm') || t.length < 2) return null;
  const num = t.slice(0, -1);
  if (!/^[0-9.]+$/.test(num) || !/[0-9]/.test(num[0]!)) return null;
  return `${num}${unit.toUpperCase()}`;
}

function isVersion(t: string): boolean {
  return t.length > 0 && /[0-9]/.test(t[0]!) && [...t].every((c) => /[0-9.]/.test(c)) && !t.endsWith('.');
}

function capitalise(w: string): string {
  return w.charAt(0).toUpperCase() + w.slice(1);
}

/** The default short name of one local model: "qwen3.8-flash-next-iq3_s" reads "Qwen 3.8". */
export function prettyModelName(model: string): string {
  // The last path segment: `hf.co/org/name:tag` is `name:tag`.
  let name = (model.split('/').pop() ?? model).toLowerCase();
  if (name.endsWith('.gguf')) name = name.slice(0, -'.gguf'.length);
  let tokens = name.split(/[-: @]/).filter(Boolean);
  let family = '';
  let version: string | null = null;
  const first = tokens[0];
  if (first !== undefined) {
    // `qwen3.8` is the family `qwen` and the version `3.8`.
    const found = first.search(/[0-9]/);
    const at = found < 0 ? first.length : found;
    const letters = first.slice(0, at);
    const rest = first.slice(at);
    if (letters) {
      family = letters.replace(/[._]+$/, '');
      if (isVersion(rest)) version = rest;
      else if (rest) family = first; // `qwen3_5` or `phi4mini`: keep the whole token as written.
      tokens = tokens.slice(1);
    }
  }
  // `gpt-oss-20b` has no version in its first word; `llama-3.2-3b` has one in its second.
  if (version === null && tokens[0] !== undefined && isVersion(tokens[0])) version = tokens.shift()!;
  let size: string | null = null;
  let inQuant = false;
  for (const t of tokens) {
    if (isQuant(t)) {
      inQuant = true;
      continue;
    }
    // The `k` and `m` of `q4-k-m` when a quantisation is written with dashes, and filler.
    if ((inQuant && t.length <= 2) || NOISE.includes(t)) continue;
    if (size === null && sizeOf(t)) {
      size = sizeOf(t);
      continue;
    }
    inQuant = false;
    // Everything else only distinguishes this model from its siblings; the short name leaves it out.
  }
  const base = [family ? capitalise(family) : null, version, size].filter((x): x is string => !!x);
  // Nothing to go on: the name as the server gives it.
  return base.length ? base.join(' ') : model;
}

/** A local model id (`local/<server>/<model>`, the model part may hold `/` and `:`) by its default short name. */
export function prettyLocalModelId(id: string): string {
  const rest = isLocalModelId(id) ? id.slice('local/'.length) : id;
  const at = rest.indexOf('/');
  return prettyModelName(at >= 0 ? rest.slice(at + 1) : rest);
}
