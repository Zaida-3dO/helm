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
