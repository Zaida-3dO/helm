// The schedules settings pane: one registerSettingsPane line.

import { createElement } from 'react';
import { registerSettingsPane } from '../registry.ts';
import { SchedulesPane } from './SchedulesPane.tsx';

registerSettingsPane({ id: 'schedules', title: 'Schedules', order: 60, render: () => createElement(SchedulesPane) });
