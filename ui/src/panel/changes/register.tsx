// Fills the Inspector's Changes slot. Imported once for its side effect by the panel's index.
import { registerPanelSlot } from '../registry.ts';
import { ChangesSlot } from './ChangesSlot.tsx';

registerPanelSlot('changes', { title: 'Changes', render: ({ chatId, visible }) => <ChangesSlot chatId={chatId} visible={visible} /> });
