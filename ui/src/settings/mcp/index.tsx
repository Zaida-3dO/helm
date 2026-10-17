// The MCP settings pane: one registerSettingsPane line.

import { createElement } from 'react';
import { registerSettingsPane } from '../registry.ts';
import { McpPane } from './McpPane.tsx';
import './mcp.css';

registerSettingsPane({ id: 'mcp', title: 'MCP', order: 40, render: () => createElement(McpPane) });
