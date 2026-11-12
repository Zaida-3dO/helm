import { test } from 'node:test';
import assert from 'node:assert/strict';
import { isSignInError } from './signin.ts';

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
