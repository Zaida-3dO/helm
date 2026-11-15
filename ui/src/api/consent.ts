// The consent ops: the card list and the user's decision. Main window only.
// For a chat that runs on another device of the household, the same two reads go through the
// devices crate: the list is this PC's copy of the cards, and the decision travels to the PC
// that runs the chat, where the first answer wins.

import type { Api } from './client.ts';
import type { ConsentDecideArgs, ConsentList, ConsentRequest } from './generated/consent/index.ts';

export type { ConsentList, ConsentRequest } from './generated/consent/index.ts';

export function consentOps(api: Api, remoteChatId?: string) {
  if (remoteChatId) {
    return {
      list: (chatId: string) => api<ConsentList>('devices_consent_list', { chatId }),
      decide: (args: ConsentDecideArgs) =>
        api<ConsentRequest>('devices_consent_decide', { ...args, chatId: remoteChatId }),
    };
  }
  return {
    list: (chatId: string) => api<ConsentList>('consent_list', { chatId }),
    decide: (args: ConsentDecideArgs) => api<ConsentRequest>('consent_decide', args),
  };
}
