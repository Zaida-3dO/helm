// The consent ops: the card list and the user's decision. Main window only.

import type { Api } from './client.ts';
import type { ConsentDecideArgs, ConsentList, ConsentRequest } from './generated/consent/index.ts';

export type { ConsentList, ConsentRequest } from './generated/consent/index.ts';

export function consentOps(api: Api) {
  return {
    list: (chatId: string) => api<ConsentList>('consent_list', { chatId }),
    decide: (args: ConsentDecideArgs) => api<ConsentRequest>('consent_decide', args),
  };
}
