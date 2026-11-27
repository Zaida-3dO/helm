"""Render the catalogue lines (error_lines.json) into flac files.

One command per voice change. Run it on the machine where the voice service
listens and where Home Assistant's ``www`` folder is reachable::

    python render_error_lines.py --www /path/to/config/www

For every entry whose file is missing: synthesise the words over the local
speech service (``POST /v1/audio/speech``, wav out), convert with ffmpeg, and
write ``<www>/<file>``. Existing files are never touched unless you pass
``--force`` - which matters for the one reused older line whose flac is shared
with a pre-existing automation. A voice change means: edit the voice file (or
pass ``--voice``) and rerun with ``--force``.

``--check`` only lists what is missing; it exits non-zero when anything is, so
it doubles as the acceptance gate ("nothing missing").

Standard library only. The speech call goes through urllib and ffmpeg runs as
an argv list (never a shell). The speech url is checked to be loopback: this
tool renders from a voice service on this machine - it never reaches out to
the house network, and no house address belongs in this repo anyway.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import urllib.error
import urllib.parse
import urllib.request

try:  # inside the package or a synthetic test package
    from . import errorlines
except ImportError:  # run as a plain script from this folder
    import errorlines

DEFAULT_TTS_URL = "http://127.0.0.1:18960"
# The shipped default lives beside the voice service's own folder, resolved
# against the current directory; pass --voice (or --voice-file) when running
# from elsewhere. An absolute default would hardcode one person's account
# path into the repo, which is exactly what must not live here.
DEFAULT_VOICE_FILE = os.path.join("voice-svc", "tts", "default_voice.txt")
LOOPBACK_HOSTS = {"127.0.0.1", "localhost", "::1"}


def _read_voice(args):
    if args.voice:
        return args.voice.strip()
    try:
        with open(args.voice_file, encoding="utf-8") as fh:
            name = fh.readline().strip()
    except OSError as exc:
        raise SystemExit("no voice: %s not readable (%s); pass --voice"
                         % (args.voice_file, exc))
    if not name:
        raise SystemExit("no voice: %s is empty; pass --voice" % args.voice_file)
    return name


def _check_loopback(url):
    host = urllib.parse.urlparse(url).hostname or ""
    if host not in LOOPBACK_HOSTS:
        raise SystemExit("refusing %s: the speech service must be on this "
                         "machine (127.0.0.1, localhost or ::1)" % url)
    return url


def synthesise_wav(tts_url, text, voice, timeout=60.0):
    """One line of words as wav bytes, from the local speech service."""
    body = json.dumps({"model": "piper", "input": text, "voice": voice,
                       "response_format": "wav"}).encode("utf-8")
    req = urllib.request.Request(tts_url.rstrip("/") + "/v1/audio/speech",
                                 data=body, method="POST",
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        raw = resp.read()
    if not raw.startswith(b"RIFF"):
        raise SystemExit("the speech service answered %d bytes that are not a "
                         "wav - is the voice name right?" % len(raw))
    return raw


def wav_to_flac(wav_bytes, out_path):
    """wav -> flac through ffmpeg (argv list, no shell). Written beside the
    target and moved over it: a half-written file on a share would play as
    noise forever."""
    ffmpeg = shutil.which("ffmpeg")
    if ffmpeg is None:
        raise SystemExit("ffmpeg not found on PATH - install it or add its "
                         "folder before rendering")
    tmp_dir = tempfile.mkdtemp(prefix="helm_lines_")
    # the part file keeps the .flac extension (ffmpeg picks the output format
    # by suffix) but stays in the target's own folder: a rename across
    # volumes is no rename at all
    part = out_path + ".part.flac"
    try:
        wav_path = os.path.join(tmp_dir, "line.wav")
        with open(wav_path, "wb") as fh:
            fh.write(wav_bytes)
        subprocess.run([ffmpeg, "-y", "-loglevel", "error", "-i", wav_path,
                        part], check=True)
        os.replace(part, out_path)
    except BaseException:
        try:
            os.unlink(part)
        except OSError:
            pass
        raise
    finally:
        shutil.rmtree(tmp_dir, ignore_errors=True)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--www", required=True,
                    help="Home Assistant www folder the flac files go into")
    ap.add_argument("--tts-url", default=DEFAULT_TTS_URL,
                    help="local speech service base url (loopback only)")
    ap.add_argument("--voice-file", default=DEFAULT_VOICE_FILE,
                    help="file whose first line names the current voice")
    ap.add_argument("--voice", help="the voice name, over --voice-file")
    ap.add_argument("--check", action="store_true",
                    help="only list lines whose file is missing; change nothing")
    ap.add_argument("--force", action="store_true",
                    help="re-render files that already exist (voice change)")
    args = ap.parse_args(argv)

    catalogue = errorlines.lines()
    if not catalogue:
        raise SystemExit("error_lines.json is missing or unreadable - nothing "
                         "to render")
    tts_url = _check_loopback(args.tts_url)

    want = [(code, entry) for code, entry in catalogue.items()]
    total = len(want)
    if args.check:
        missing = [entry["file"] for _, entry in want
                   if not os.path.isfile(os.path.join(args.www, entry["file"]))]
        for name in missing:
            print("MISSING %s" % name)
        if missing:
            print("%d of %d lines are missing from %s"
                  % (len(missing), total, args.www))
            return 1
        print("all %d line files are present in %s" % (total, args.www))
        return 0

    voice = _read_voice(args)
    os.makedirs(args.www, exist_ok=True)
    done = skipped = 0
    for index, (code, entry) in enumerate(want, 1):
        target = os.path.join(args.www, entry["file"])
        if os.path.isfile(target) and not args.force:
            skipped += 1
            print("skip %s (%d of %d)" % (entry["file"], index, total))
            continue
        wav = synthesise_wav(tts_url, entry["text"], voice)
        wav_to_flac(wav, target)
        done += 1
        print("rendered %s (%d of %d)" % (entry["file"], index, total))
    print("%d rendered, %d skipped, %d lines in the catalogue"
          % (done, skipped, total))
    return 0


if __name__ == "__main__":
    sys.exit(main())
