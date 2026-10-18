// Registrations for the orchestrator: the Orchestrator switch in the composer toolbar and the workflow
// panel in the composer dock.

import { createElement } from 'react';
import { registerComposerAccessory } from '../composer/slots.ts';
import { ModeSwitch } from './ModeSwitch.tsx';
import { WorkflowPanel } from './WorkflowPanel.tsx';

registerComposerAccessory({
  id: 'orchestrator-switch',
  placement: 'toolbar',
  render: (ctx) => createElement(ModeSwitch, { ctx }),
});

registerComposerAccessory({
  id: 'workflow-panel',
  placement: 'dock',
  render: (ctx) => createElement(WorkflowPanel, { ctx }),
});
