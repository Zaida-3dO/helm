// The queue settings pane: one registerSettingsPane line.

import { createElement } from 'react';
import { registerSettingsPane } from '../registry.ts';
import { QueueSettings } from './QueuePane.tsx';
import '../../queue/queue.css';

registerSettingsPane({ id: 'queue', title: 'Queue', order: 52, render: () => createElement(QueueSettings) });
