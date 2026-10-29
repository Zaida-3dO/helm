// Settings → Web search: one registerSettingsPane line.

import { createElement } from 'react';
import { registerSettingsPane } from '../registry.ts';
import { WebSearchPane } from './WebSearchPane.tsx';

registerSettingsPane({ id: 'websearch', title: 'Web search', order: 46, render: () => createElement(WebSearchPane) });
