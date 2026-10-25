// UI registrations for AFK: the Settings → AFK and Usage governor panes, the AFK pill in the composer
// toolbar, and the presence reporter that App mounts once.

import { createElement } from 'react';
import { registerComposerAccessory } from '../composer/slots.ts';
import { registerSettingsPane } from '../settings/registry.ts';
import { AfkPane } from './AfkPane.tsx';
import { AfkPill } from './AfkPill.tsx';
import { GovernorPane } from './GovernorPane.tsx';

export { AfkPresence } from './AfkPresence.tsx';

registerSettingsPane({ id: 'afk', title: 'AFK', order: 60, render: () => createElement(AfkPane) });
registerSettingsPane({ id: 'governor', title: 'Usage governor', order: 55, render: () => createElement(GovernorPane) });
registerComposerAccessory({ id: 'afk-pill', placement: 'toolbar', render: () => createElement(AfkPill) });
