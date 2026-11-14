// Settings → Gatekeeper: one registerSettingsPane line.

import { createElement } from 'react';
import { registerSettingsPane } from '../registry.ts';
import { GatekeeperPane } from './GatekeeperPane.tsx';

registerSettingsPane({ id: 'gatekeeper', title: 'Gatekeeper', order: 58, render: () => createElement(GatekeeperPane) });
