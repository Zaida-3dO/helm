import { test } from 'node:test';
import assert from 'node:assert/strict';
import { isSignInBusyError, isSignInError } from './signin.ts';

test('only an expired or missing Claude sign-in offers to sign in again', () => {
  const rows: Array<[string, boolean]> = [
    ['Failed to authenticate: OAuth session expired and could not be refreshed', true],
    ['Failed to authenticate. API Error: 401 {"type":"error","error":{"type":"authentication_error","message":"OAuth access token has expired"}}', true],
    ['OAuth session expired', true],
    ['The OAuth token has expired. Run claude auth login.', true],
    ['OAuth access token has expired', true],
    ['Not logged in · Please run /login', true],
    ['Please run /login', true],
    ['Invalid API key · Please run /login', true],
    ['API Error: 401 authentication_error', true],
    ['authentication_failed', true],
    ['FAILED TO AUTHENTICATE', true],
    // Near misses.
    ['MCP server linear: OAuth sign-in failed. Reconnect it in Settings.', false],
    ['GitHub did not accept the sign-in', false],
    ['Invalid API key', false],
    ['API Error: 529 {"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}', false],
    ['API Error: 500 Internal server error', false],
    ['Prompt is too long', false],
    ['The conversation is too long for this model; start a new chat.', false],
    ['', false],
  ];
  for (const [text, want] of rows) assert.equal(isSignInError(text), want, text);
  assert.equal(isSignInError(null), false);
  assert.equal(isSignInError(undefined), false);
});

test('a refresh collision reads as a busy sign-in, an expired one as a sign-in', () => {
  const rows: Array<[string, boolean]> = [
    // A refresh another process was already doing: busy, not broken.
    ['Failed to refresh OAuth token: another Claude Code process is refreshing it, please try again', true],
    ["Claude's sign-in was busy (another Claude Code process was refreshing it). Helm tries again by itself.", true],
    // Near misses: these are expired or refused sign-ins, not a collision.
    ['Failed to authenticate: OAuth session expired and could not be refreshed', false],
    ['The OAuth token has expired. Run claude auth login.', false],
    ['OAuth session expired', false],
    ['MCP server linear: OAuth sign-in failed. Reconnect it in Settings.', false],
    ['', false],
  ];
  for (const [text, want] of rows) assert.equal(isSignInBusyError(text), want, text);
  assert.equal(isSignInBusyError(null), false);
  assert.equal(isSignInBusyError(undefined), false);
  // The two never overlap: one predicate's text is the other's near miss.
  assert.equal(isSignInError('Failed to refresh OAuth token: another Claude Code process is refreshing it, please try again'), false);
  assert.equal(isSignInBusyError('OAuth session expired'), false);
});
