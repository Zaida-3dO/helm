// The orchestrator settings pane: one registerSettingsPane line.

import { createElement } from 'react';
import { registerSettingsPane } from '../registry.ts';
import { OrchestratorPane } from './OrchestratorPane.tsx';

registerSettingsPane({ id: 'orchestrator', title: 'Orchestrator', order: 70, render: () => createElement(OrchestratorPane) });
