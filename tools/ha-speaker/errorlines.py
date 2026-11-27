"""Pre-recorded spoken lines for turns that fail or end without an answer.

One data file - ``error_lines.json``, next to this module - is the single
source for every line: its code, the words we speak, the flac the voice
service renders into Home Assistant's ``www`` folder, when it plays, and
which fixed sentence of this package it stands in for.

Division of labour: this component only decides which code applies at the end
of a turn (one function, one table). It never plays anything itself - an
empty spoken string makes the voice pipeline stop, and a Home Assistant
automation announces the flac file. A delayed announcement from here would
cancel our own running pipeline; that is why playback lives in the automation.

Standard library only, no Home Assistant imports, and reading the json never
raises: when it is missing or unreadable there are simply no lines and every
turn speaks live words, exactly as before this feature existed.
"""

import json
import os

try:  # inside the package (Home Assistant) or a synthetic test package
    from . import agent_core
except ImportError:  # run as plain modules from this folder (unit tests)
    import agent_core


# The hub's own words for being too old to answer by voice. They arrive as a
# normal answer, so recognising them is a string comparison - fragile by
# nature and accepted as such: if the hub ever rewords the sentence, we fall
# back to speaking its live words instead of playing the file.
NEED_UPDATE_TEXT = "Helm needs updating before I can answer by voice."

_LINES_FILE = "error_lines.json"
_lines_cache = None


def lines():
    """The line list keyed by code. {} when the file is missing or broken -
    which turns the whole feature off and restores live speech."""
    global _lines_cache  # noqa: PLW0603 - read once per process; a changed
    # file needs an integration reload, the same rule as the code itself.
    if _lines_cache is not None:
        return _lines_cache
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), _LINES_FILE)
    try:
        with open(path, encoding="utf-8") as fh:
            raw = json.load(fh)
    except (OSError, ValueError):
        raw = None
    keyed = {}
    if isinstance(raw, list):
        for entry in raw:
            if not isinstance(entry, dict):
                continue
            code = entry.get("code")
            text = entry.get("text")
            file_name = entry.get("file")
            if (isinstance(code, str) and code.strip()
                    and isinstance(text, str) and text.strip()
                    and isinstance(file_name, str) and file_name.strip()):
                keyed[code] = entry
    _lines_cache = keyed
    return keyed


def _escalate_code(status, state, answer_text):
    """How the hub side of a turn ended: an offline hub is its own sentence;
    busy is told apart by http status; the too-old-hub answer matches words."""
    if state == "offline":
        return "cannot_reach_helm"
    if state == "timeout":
        return "answer_on_phone"
    if status == 429:
        return "too_busy"
    if isinstance(answer_text, str) and answer_text.strip() == NEED_UPDATE_TEXT:
        return "helm_needs_updating"
    if state in ("error", "bad_auth"):
        # a wrong token or scope is a setup mistake worth grepping for; the
        # caller logs it. Out loud it is only ever "try again".
        return "try_again"
    return None


def code_for_turn(kind, text=None, has_satellite=True, escalate_status=None,
                  escalate_state=None, escalate_text=None):
    """The one table: given how a turn ended, the line to play - or None when
    live words should be spoken as before.

    Main-endpoint failures arrive as ``kind`` plus the fixed sentence that
    would have been spoken (it tells "empty" from "unreliable" from a blocked
    house request). Hub-side failures carry the hub's result state, its http
    status where one exists, and the hub's answer text for an exact-match line.
    ``has_satellite`` is False when the turn has no asking satellite device in
    front of it (text or REST calls): there is nobody to announce to, so live
    words as before. A code with no json entry also returns None: no file, no
    announcement."""
    if not has_satellite:
        return None
    if escalate_state is not None:
        code = _escalate_code(escalate_status, escalate_state, escalate_text)
    elif kind == agent_core.KIND_OFFLINE:
        code = "thinking_offline"
    elif kind == agent_core.KIND_TIMEOUT:
        code = "took_too_long"
    elif kind == agent_core.KIND_ERROR:
        if text == agent_core.EMPTY_TEXT:
            code = "did_not_catch_that"
        elif text == agent_core.CONTROL_TEXT:
            code = "could_not_do_that"
        else:
            code = "try_again"
    else:  # answer, escalate-with-no-hub, anything unexpected: live words
        return None
    return code if code in lines() else None


def filename_for(code):
    """The flac file name for a code (from json; the default when an entry
    forgets to override)."""
    entry = lines().get(code)
    if entry is None:
        return "helm_speaker_err_%s.flac" % code
    return entry["file"]


def canonical_text(code):
    """The spoken words behind a code, exactly as the json has them - what a
    history row or hub record keeps when the flac replaces live speech."""
    entry = lines().get(code)
    return entry["text"] if entry else None
