// Record shapes the UI consumes, re-exported from the generated bindings
// (ui/src/api/generated/, ts-rs). The UI imports from here so the generated folder stays the one
// source of truth, plus the few names the UI adds on top.

import type { SendBehavior } from './generated/index.ts';

export type {
  AttentionKind,
  AttentionRow,
  AttentionSummary,
  BackgroundTask,
  Capabilities,
  ChatEvent,
  ChatRecord,
  ChatStatus,
  Completion,
  ContextKind,
  Interaction,
  LiveTool,
  MessageDetails,
  MessageRole,
  MessageRow,
  MessageStatus,
  ModelCatalogue,
  RetryState,
  RunSummary as Run,
  SendAction,
  SendBehavior,
  SendMode,
  SendOrigin,
  Submission,
  SubmissionActionKind as SubmissionAction,
  SubmissionState,
  ThreadState,
  TitleState,
  TranscriptPage,
  UnreadState,
} from './generated/index.ts';

/**
 * The default send behaviour, mirroring host/crates/chats send_behavior.rs. The host always
 * fills them in; the UI uses them only before the first thread state arrives.
 */
export const HOME_SEND_BEHAVIOR: SendBehavior = {
  mode: 'asap',
  showChoices: true,
  stopSendsWaiting: false,
  waitLimitSeconds: 30,
  toolStop: true,
  longCommands: 'redirect',
};
