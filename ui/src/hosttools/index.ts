// UI registrations for the host tools: the Allow cards in the composer dock's consent position.

import { createElement } from 'react';
import { registerComposerAccessory } from '../composer/slots.ts';
import { AllowCards } from './AllowCards.tsx';

registerComposerAccessory({
  id: 'allow-cards',
  placement: 'consent',
  render: (ctx) => createElement(AllowCards, { chatId: ctx.chat.id }),
});
