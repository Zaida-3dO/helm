// The dev fake host's new-chat ops, for checking the page in a plain browser.
// fakeHost.ts registers it through `registerFakeOps`; this file only imports types from there.

import type { FakeOpHandler } from '../dev/fakeHost.ts';

/** The folder the fake "choose a folder" dialog answers with. */
export const FAKE_PICKED_FOLDER = 'C:/Users/me/Documents/Coding/picked';

export function newChatFakeOps(): { prefixes: string[]; handler: FakeOpHandler } {
  const handler: FakeOpHandler = (op) => {
    if (op === 'folder_pick') return { path: FAKE_PICKED_FOLDER };
    throw { code: 'not_implemented', message: `The fake host has no op ${op}.` };
  };
  return { prefixes: ['folder_pick'], handler };
}
