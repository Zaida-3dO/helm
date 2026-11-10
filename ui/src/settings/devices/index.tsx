// Settings → Devices: one registerSettingsPane line.

import { createElement } from 'react';
import { registerSettingsPane } from '../registry.ts';
import { DevicesPane } from './DevicesPane.tsx';

registerSettingsPane({ id: 'devices', title: 'Devices', order: 43, render: () => createElement(DevicesPane) });
