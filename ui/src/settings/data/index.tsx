// Settings → Data: one registerSettingsPane line.

import { createElement } from 'react';
import { registerSettingsPane } from '../registry.ts';
import { DataPane } from './DataPane.tsx';

registerSettingsPane({ id: 'data', title: 'Data', order: 48, render: () => createElement(DataPane) });
