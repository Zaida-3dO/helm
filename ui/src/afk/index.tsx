// UI registrations for AFK: the Settings → AFK and Usage governor panes, and the presence reporter that
// App mounts once.

import { createElement } from 'react';
import { registerSettingsPane } from '../settings/registry.ts';
import { AfkPane } from './AfkPane.tsx';
import { GovernorPane } from './GovernorPane.tsx';

export { AfkPresence } from './AfkPresence.tsx';

registerSettingsPane({ id: 'afk', title: 'AFK', order: 60, render: () => createElement(AfkPane) });
registerSettingsPane({ id: 'governor', title: 'Usage governor', order: 55, render: () => createElement(GovernorPane) });
