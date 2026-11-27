"""Tests for render_error_lines.py: a stdlib fake speech service plus a fake
ffmpeg, rendering into a temp www folder. No real voice service, no ffmpeg
install, no network beyond loopback.

Run with plain python:  python -m unittest test_error_lines

The fake ffmpeg replaces subprocess.run rather than sitting on PATH as a
script: Windows cannot spawn a .cmd shim without a shell (and this tool must
never use one), so the replacement answers the exact argv list the tool
builds and copies wav to flac itself. A real one-command render against the
real voice service is an acceptance step, run by hand outside this suite.
"""

import http.server
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import types
import unittest
from contextlib import redirect_stdout

import errorlines
import render_error_lines as render


class FakeSpeech(http.server.BaseHTTPRequestHandler):
    payload = b"RIFFfake-wav-payload"
    calls = []

    def do_POST(self):  # noqa: N802 - handler naming is http.server's
        length = int(self.headers.get("Content-Length", "0"))
        body = json.loads(self.rfile.read(length).decode("utf-8"))
        FakeSpeech.calls.append((self.path, body))
        self.send_response(200)
        self.send_header("Content-Type", "audio/wav")
        self.send_header("Content-Length", str(len(FakeSpeech.payload)))
        self.end_headers()
        self.wfile.write(FakeSpeech.payload)

    def log_message(self, *args):  # keep the test output clean
        pass


class FakeFfmpeg:
    """Stands in for subprocess.run: checks the argv shape and copies the
    wav over to where the flac belongs."""

    def __init__(self):
        self.cmds = []

    def __call__(self, argv, check=False, **kwargs):
        self.cmds.append(list(argv))
        base = os.path.basename(str(argv[0])).lower()
        if not base.startswith("ffmpeg") or "-i" not in argv:
            raise AssertionError("expected an ffmpeg argv list, got %r" % argv)
        src = argv[argv.index("-i") + 1]
        dst = argv[-1]
        shutil.copyfile(src, dst)
        return subprocess.CompletedProcess(argv, 0)


class RenderFlowTest(unittest.TestCase):
    def setUp(self):
        FakeSpeech.calls = []
        FakeSpeech.payload = b"RIFFfake-wav-payload"
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), FakeSpeech)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.url = "http://127.0.0.1:%d" % self.server.server_address[1]
        self.www = tempfile.mkdtemp(prefix="helm_www_")
        self._which = shutil.which
        self._run = subprocess.run

    def tearDown(self):
        shutil.which = self._which
        subprocess.run = self._run
        self.server.shutdown()
        self.server.server_close()
        shutil.rmtree(self.www, ignore_errors=True)

    def _main(self, *extra):
        runner = FakeFfmpeg()
        shutil.which = lambda name: "/usr/bin/ffmpeg"
        subprocess.run = runner
        out = io.StringIO()
        with redirect_stdout(out):
            code = render.main(["--www", self.www, "--tts-url", self.url,
                                *extra])
        return code, out.getvalue(), runner

    def test_render_skip_check_force(self):
        catalogue = errorlines.lines()
        total = len(catalogue)
        # the temp www starts empty, so everything is new here
        code, out, runner = self._main("--voice", "tv")
        self.assertEqual(code, 0)
        self.assertEqual(len(FakeSpeech.calls), total)
        self.assertEqual(len(runner.cmds), total)

        # one entry's exact speech request went over: the wire shape carries
        # words, voice and wav output
        path, body = FakeSpeech.calls[0]
        self.assertEqual(path, "/v1/audio/speech")
        self.assertEqual(body["response_format"], "wav")
        self.assertEqual(body["voice"], "tv")
        self.assertEqual(body["model"], "piper")
        for _code, entry in catalogue.items():
            self.assertTrue(os.path.isfile(os.path.join(self.www, entry["file"])),
                            entry["file"])

        # second run touches nothing: the sentinel stays, no new calls
        entry = catalogue[sorted(catalogue)[0]]
        target = os.path.join(self.www, entry["file"])
        with open(target, "wb") as fh:
            fh.write(b"keep me")
        code, out, _runner = self._main("--voice", "tv")
        self.assertEqual(code, 0)
        # every entry reports a skip; iteration follows json order, this test's
        # sentinel file is not necessarily the first line printed
        self.assertIn("skip %s (" % entry["file"], out)
        self.assertIn("%d skipped, %d lines" % (total, total), out)
        self.assertEqual(len(FakeSpeech.calls), total)
        with open(target, "rb") as fh:
            self.assertEqual(fh.read(), b"keep me")

        # --check finds a hole, rendering closes exactly it
        os.unlink(target)
        code, out = self._capture_main(["--check"])
        self.assertEqual(code, 1)
        self.assertIn("MISSING %s" % entry["file"], out)
        base = len(FakeSpeech.calls)
        code, out, _runner = self._main("--voice", "tv")
        self.assertEqual(len(FakeSpeech.calls), base + 1)
        with open(target, "rb") as fh:
            self.assertEqual(fh.read(), FakeSpeech.payload)
        code, out = self._capture_main(["--check"])
        self.assertEqual(code, 0)
        self.assertIn("all %d line files are present" % total, out)

        # --force re-renders every line (the voice-change path)
        code, out, _runner = self._main("--voice", "tv", "--force")
        self.assertEqual(len(FakeSpeech.calls), base + 1 + total)

    def _capture_main(self, extra):
        runner = FakeFfmpeg()
        shutil.which = lambda name: "/usr/bin/ffmpeg"
        subprocess.run = runner
        out = io.StringIO()
        with redirect_stdout(out):
            code = render.main(["--www", self.www, "--tts-url", self.url, *extra])
        return code, out.getvalue()

    def test_guard_refuses_a_non_loopback_service(self):
        with self.assertRaises(SystemExit) as caught:
            render._check_loopback("http://192.168.9.101:18960")
        self.assertIn("this machine", str(caught.exception).lower())
        self.assertTrue(render._check_loopback("http://localhost:18960"))

    def test_voice_comes_from_flag_or_file(self):
        ns = types.SimpleNamespace(voice=None, voice_file="missing-file.txt")
        with self.assertRaises(SystemExit):
            render._read_voice(ns)
        with tempfile.NamedTemporaryFile("w", suffix=".txt", delete=False,
                                         encoding="utf-8") as fh:
            fh.write("en_ZZ-tester-medium\n")
            path = fh.name
        try:
            ns.voice_file = path
            self.assertEqual(render._read_voice(ns), "en_ZZ-tester-medium")
            self.assertEqual(render._read_voice(types.SimpleNamespace(
                voice=" chosen ", voice_file=path)), "chosen")
        finally:
            os.unlink(path)

    def test_a_non_wav_answer_is_an_error_not_silence(self):
        FakeSpeech.payload = b"<html>voice service blew up</html>"
        with self.assertRaises(SystemExit) as caught:
            render.synthesise_wav(self.url, "hello", "tv")
        self.assertIn("not a", str(caught.exception).lower())


if __name__ == "__main__":
    unittest.main()
