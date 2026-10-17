// The projects settings pane: one registerSettingsPane line.

import { createElement } from 'react';
import { registerSettingsPane } from '../registry.ts';
import { ProjectsPane } from './ProjectsPane.tsx';
import './projects.css';

registerSettingsPane({ id: 'projects', title: 'Projects', order: 20, render: () => createElement(ProjectsPane) });
