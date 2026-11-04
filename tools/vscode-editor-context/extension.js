// Writes the editor's open file and selection to a plain file in Helm's profile folder, so an agent
// in Helm can read it (only when the user has turned that on in Helm's settings).
//
// One small JSON file per editor window, `<profile>/ide-context/<window>.json`. It is rewritten when
// the active file or the selection changes, touched every HEARTBEAT_MS so Helm can tell a live window
// from a crashed one, and deleted when the window closes. Nothing is sent anywhere else.
// No build step and no dependencies: VS Code runs this file as it is.

'use strict';

const vscode = require('vscode');
const fs = require('fs');
const os = require('os');
const path = require('path');
const crypto = require('crypto');

const FOLDER = 'ide-context';
const HEARTBEAT_MS = 30000;
const WRITE_DELAY_MS = 150;
// Helm cuts a selection at 12 KiB itself; sending a little more keeps the cut in one place.
const MAX_SELECTION_CHARS = 16000;
const MAX_FOLDERS = 16;

/** Helm's profile folder, by the same rule Helm uses. */
function profileDir() {
  if (process.env.HELM_PROFILE_DIR) return process.env.HELM_PROFILE_DIR;
  if (process.platform === 'win32') {
    return process.env.LOCALAPPDATA ? path.join(process.env.LOCALAPPDATA, 'Helm') : null;
  }
  const base = process.env.XDG_DATA_HOME || path.join(os.homedir(), '.local', 'share');
  return path.join(base, 'helm');
}

/** Where to write: the setting if set, else `ide-context` in Helm's profile folder (which must already exist). */
function targetFolder() {
  const custom = vscode.workspace.getConfiguration('helmEditorContext').get('folder');
  if (typeof custom === 'string' && custom.trim()) return custom.trim();
  const profile = profileDir();
  // Helm creates its own profile folder; never create it here.
  if (!profile || !fs.existsSync(profile)) return null;
  return path.join(profile, FOLDER);
}

function activate(context) {
  const windowId = crypto.randomBytes(8).toString('hex');
  let editor = vscode.window.activeTextEditor;
  let timer = null;
  let file = null;

  const snapshot = () => {
    const folders = (vscode.workspace.workspaceFolders || [])
      .map((f) => f.uri.fsPath)
      .slice(0, MAX_FOLDERS);
    const doc = editor && editor.document;
    const onDisk = doc && doc.uri.scheme === 'file';
    let selection = null;
    if (onDisk && editor.selection && !editor.selection.isEmpty) {
      const sel = editor.selection;
      // Lines count from 1. A selection that ends at the very start of a line stops on the line before.
      const lastLine = sel.end.character === 0 && sel.end.line > sel.start.line ? sel.end.line - 1 : sel.end.line;
      selection = {
        filePath: doc.uri.fsPath,
        startLine: sel.start.line + 1,
        endLine: lastLine + 1,
        text: doc.getText(sel).slice(0, MAX_SELECTION_CHARS),
      };
    }
    return {
      version: 1,
      editor: vscode.env.appName || 'Visual Studio Code',
      workspaceFolders: folders,
      activeFile: onDisk ? doc.uri.fsPath : null,
      language: doc ? doc.languageId : null,
      selection,
    };
  };

  const write = () => {
    timer = null;
    const dir = targetFolder();
    if (!dir) return;
    try {
      fs.mkdirSync(dir, { recursive: true });
      file = path.join(dir, windowId + '.json');
      const tmp = file + '.tmp';
      fs.writeFileSync(tmp, JSON.stringify(snapshot()));
      fs.renameSync(tmp, file);
    } catch (_) {
      // Helm may not be installed or the folder may not be writable: try again at the next change.
    }
  };

  const soon = () => {
    if (!timer) timer = setTimeout(write, WRITE_DELAY_MS);
  };

  const touch = () => {
    if (!file) return write();
    try {
      const now = new Date();
      fs.utimesSync(file, now, now);
    } catch (_) {
      write();
    }
  };

  const remove = () => {
    if (!file) return;
    try {
      fs.unlinkSync(file);
    } catch (_) {
      // Already gone.
    }
    file = null;
  };

  context.subscriptions.push(
    // Keep the last text editor when focus moves to a panel or a webview: the user usually selects
    // something, then switches to Helm to ask about it.
    vscode.window.onDidChangeActiveTextEditor((e) => {
      if (e) {
        editor = e;
        soon();
      }
    }),
    vscode.window.onDidChangeTextEditorSelection((e) => {
      if (e.textEditor === editor) soon();
    }),
    vscode.workspace.onDidChangeWorkspaceFolders(soon),
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration('helmEditorContext')) {
        remove();
        write();
      }
    }),
    { dispose: () => timer && clearTimeout(timer) },
    { dispose: remove },
  );
  const beat = setInterval(touch, HEARTBEAT_MS);
  context.subscriptions.push({ dispose: () => clearInterval(beat) });
  write();
}

function deactivate() {}

module.exports = { activate, deactivate };
