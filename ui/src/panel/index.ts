// The side panel's public surface. Import from here, not from the files.
// The built-in kinds (file tabs, the Files slot) register themselves on import.
import './viewer/kinds.tsx';
export { SidePanel } from './SidePanel.tsx';
export type { SidePanelProps } from './SidePanel.tsx';
export {
  isPanelVisible,
  openInPanel,
  panelStore,
  registerPanelKind,
  registerPanelSlot,
  showSlot,
  togglePanel,
} from './registry.ts';
export type { PanelKind, PanelSlot, TabContext } from './registry.ts';
export type { CloseReason, Intent, PanelTab, SlotId } from './model.ts';
export { gateTarget, handleLink, registerLinkRoute } from './links.ts';
export type { GateResult, LinkContext, LinkOutcome } from './links.ts';
