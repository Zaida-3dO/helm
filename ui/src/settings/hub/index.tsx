// Settings → Hub: one registerSettingsPane line.

import { createElement } from 'react';
import { registerSettingsPane } from '../registry.ts';
import { HubPane } from './HubPane.tsx';

registerSettingsPane({ id: 'hub', title: 'Hub', order: 42, render: () => createElement(HubPane) });
