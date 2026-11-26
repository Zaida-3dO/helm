"""One JSON line per turn, appended to a rotating log.

The benchmark reads this file; every record is plain data: no tokens, no
entity ids beyond what the caller chose to put in tags. Rotation keeps the
file bounded: when it passes max_bytes the current file becomes ``*.1``
(overwriting the previous rotation).
"""

import json
import os

MAX_BYTES = 5 * 1024 * 1024


class TimingWriter:
    """Append records as JSON lines; rotate at max_bytes."""

    def __init__(self, path, max_bytes=MAX_BYTES):
        self.path = path
        self.max_bytes = max_bytes

    def append(self, record):
        line = json.dumps(record, ensure_ascii=False, sort_keys=True)
        try:
            os.makedirs(os.path.dirname(self.path), exist_ok=True)
        except OSError:
            pass  # a pre-existing non-dir path or perms: the open below fails cleanly
        with open(self.path, "a", encoding="utf-8", newline="\n") as fh:
            fh.write(line + "\n")
            try:
                size = fh.tell()
            except (OSError, ValueError):
                size = 0
        if size >= self.max_bytes:
            self._rotate()

    def _rotate(self):
        try:
            os.replace(self.path, self.path + ".1")
        except OSError:
            pass  # never fail a turn over bookkeeping; the next append recreates
