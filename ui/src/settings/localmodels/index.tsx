// Settings → Local models: one registerSettingsPane line. It sits after Runtime (35) and before
// MCP (40), where the other settings about how chats run live.

import { createElement } from 'react';
import { registerSettingsPane } from '../registry.ts';
import { LocalModelsPane } from './LocalModelsPane.tsx';

registerSettingsPane({ id: 'localmodels', title: 'Local models', order: 36, render: () => createElement(LocalModelsPane) });
