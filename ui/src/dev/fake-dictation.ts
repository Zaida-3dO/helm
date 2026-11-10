// The dev fake host's dictation ops (the composer's microphone and Settings → Shell → Dictation in
// a plain browser). No server is called: a recording "transcribes" to a fixed phrase, and an
// address with "down" in it plays a server that does not answer. Dev builds only.

import { ApiError } from '../api/client.ts';
import type { DictationView } from '../composer/dictation.ts';
import type { FakeOpHandler } from './fakeHost.ts';

const HEARD = 'this was dictated in the dev browser';

export function dictationFakeOps(): { prefixes: string[]; handler: FakeOpHandler } {
  const state = { sttUrl: null as string | null, allowPrivateNetwork: false, language: 'en', model: 'large-v3-turbo', reserve: 2 };
  const view = (): DictationView => ({
    sttUrl: state.sttUrl,
    allowPrivateNetwork: state.allowPrivateNetwork,
    model: state.model,
    language: state.language,
    configured: state.sttUrl !== null,
    speechReserveGb: state.reserve,
    speechReserveMaxGb: 24,
  });
  const handler: FakeOpHandler = (op, a, ctx) => {
    switch (op) {
      case 'dictation_settings_get':
        return view();
      case 'dictation_settings_set': {
        if (typeof a.allowPrivateNetwork === 'boolean') state.allowPrivateNetwork = a.allowPrivateNetwork;
        if (typeof a.sttUrl === 'string') {
          const url = a.sttUrl.trim();
          if (url && !/^http:\/\/(127\.0\.0\.1|localhost|\[::1\]|10\.|192\.168\.)/i.test(url)) {
            throw new ApiError('bad_args', 'That address is not on this computer or your own network. Use 127.0.0.1, or the numeric address (like 192.168.1.20) of a computer at home.');
          }
          state.sttUrl = url || null;
        }
        if (typeof a.language === 'string') {
          if (!/^(auto|[a-z]{2,3}(-[A-Za-z]{2,4})?)$/.test(a.language.trim())) throw new ApiError('bad_args', 'The language is a code like en or fr, or auto.');
          state.language = a.language.trim();
        }
        if (typeof a.model === 'string' && a.model.trim()) state.model = a.model.trim();
        if (typeof a.speechReserveGb === 'number') {
          if (!Number.isInteger(a.speechReserveGb) || a.speechReserveGb < 0 || a.speechReserveGb > 24) {
            throw new ApiError('bad_args', 'The memory kept for speech must be a whole number of GB from 0 to 24.');
          }
          state.reserve = a.speechReserveGb;
        }
        ctx.emit({ kind: 'dictation' });
        return view();
      }
      case 'dictation_test':
        if (!state.sttUrl) return { ok: false, message: "Dictation isn't set up yet." };
        return state.sttUrl.includes('down') ? { ok: false, message: "Speech-to-text didn't answer." } : { ok: true, message: 'The speech-to-text server answered.' };
      case 'dictation_transcribe':
        if (!state.sttUrl) throw new ApiError('not_set_up', "Dictation isn't set up yet.");
        if (state.sttUrl.includes('down')) throw new ApiError('unreachable', "Speech-to-text didn't answer.");
        return { text: HEARD };
      default:
        throw new ApiError('unknown_op', op);
    }
  };
  return { prefixes: ['dictation_'], handler };
}
