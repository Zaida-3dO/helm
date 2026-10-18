// The usage settings pane: one registerSettingsPane line.

import { createElement } from 'react';
import { registerSettingsPane } from '../registry.ts';
import { UsagePane } from './UsagePane.tsx';

registerSettingsPane({ id: 'usage', title: 'Usage', order: 50, render: () => createElement(UsagePane) });
