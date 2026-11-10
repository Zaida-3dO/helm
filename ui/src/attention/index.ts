// The Needs attention inbox. The sidebar mounts it from its nav entry and binds Ctrl+Alt+A.

export { ATTENTION_DIALOG, AttentionInbox } from './AttentionInbox.tsx';
export type { AttentionActions, AttentionInboxProps } from './AttentionInbox.tsx';
export { FlagNote } from './FlagNote.tsx';
export { countText, failedChecks, flagFor, inboxCount, inboxRows, mutedKindLabel, mutedRows, nextAttention, parseDelayMinutes, priorityFor, snoozeChoices, whenText } from './model.ts';
export type { InboxGroup, InboxRow, SnoozeChoice, TriageSettings } from './model.ts';
