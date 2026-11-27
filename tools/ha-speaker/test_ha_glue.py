"""Glue tests that need no Home Assistant: profile build, switch and
secrecy. The HA-side acceptance (entry sets up hub-less; a processed turn
calls the right light service) runs in a scratch Home Assistant install,
because those names only exist with HA loaded.

Run with either plain python or the scratch venv: profiles.py imports only
agent_core and const.
"""

import asyncio
import os
import sys
import tempfile
import types
import unittest

# profiles.py imports its siblings relatively (that is how HA loads this
# integration), so borrow a synthetic package whose __path__ points here -
# without executing __init__.py, which would need Home Assistant itself.
_HERE = os.path.dirname(os.path.abspath(__file__))
_PKG = types.ModuleType("hs")
_PKG.__path__ = [_HERE]
sys.modules.setdefault("hs", _PKG)

import importlib  # noqa: E402

def _stub_homeassistant():
    """ha_backend.py is live-HA glue; give it the handful of HA names it
    imports so house_read - which only ever touches the injected transport -
    can be tested without Home Assistant itself. Skipped when real HA is
    importable, so the scratch-HA run keeps testing the genuine modules."""
    try:  # pragma: no cover - environment dependent
        import homeassistant  # noqa: F401
        return False
    except ImportError:
        pass

    def mod(name, parent=None, **attrs):
        m = types.ModuleType(name)
        for key, value in attrs.items():
            setattr(m, key, value)
        sys.modules[name] = m
        if parent is not None:
            setattr(parent, name.rsplit(".", 1)[1], m)
        return m

    ha = mod("homeassistant")
    mod("homeassistant.core", ha, HomeAssistant=object)
    comp = mod("homeassistant.components", ha)
    comp_assist = mod("homeassistant.components.homeassistant", comp)
    mod("homeassistant.components.homeassistant.exposed_entities", comp_assist,
        async_should_expose=lambda hass, assistant, entity_id: True)
    helpers = mod("homeassistant.helpers", ha)
    for name in ("area_registry", "device_registry", "entity_registry"):
        mod("homeassistant.helpers." + name, helpers)
    mod("homeassistant.helpers.intent", helpers,
        IntentError=type("IntentError", (Exception,), {}),
        async_handle=None)
    mod("homeassistant.helpers.aiohttp_client", helpers,
        async_get_clientsession=lambda hass: None)
    return True


_stub_homeassistant()
_ha_backend = importlib.import_module("hs.ha_backend")
_errors = importlib.import_module("hs.errors")
_housecontrol = importlib.import_module("hs.homecontrol")

_agent_core = importlib.import_module("hs.agent_core")
_errorlines = importlib.import_module("hs.errorlines")
_hubapi = importlib.import_module("hs.hubapi")

_profiles = importlib.import_module("hs.profiles")
active_profile = _profiles.active_profile
build_model_config = _profiles.build_model_config
parse_json_field = _profiles.parse_json_field
profile_from_form = _profiles.profile_from_form
profiles_from_entry = _profiles.profiles_from_entry
redact = _profiles.redact


def base_data(**over):
    data = {"model_url": "http://model.example:11434/v1/chat/completions",
            "model_name": "qwen3.5:4b", "model_api_key": None,
            "thinking": True, "headers": {}, "body": {"think": True},
            "timeout_s": 30}
    data.update(over)
    return data


class ProfilesTest(unittest.TestCase):
    def test_build_from_data_with_no_options(self):
        cfg = build_model_config(base_data(), {})
        self.assertEqual(cfg.model_name, "qwen3.5:4b")
        self.assertEqual(cfg.timeout_s, 30)
        self.assertTrue(cfg.thinking)
        # the checkbox has to arrive somewhere real: it travels as the
        # request body's think switch unless the body box spells it already
        self.assertIs(cfg.extra_body.get("think"), True)
        cfg_off = build_model_config(base_data(thinking=False, body={}), {})
        # off adds nothing: strict endpoints reject unknown fields, and the
        # server default already means off for most models
        self.assertNotIn("think", cfg_off.extra_body)
        cfg_own = build_model_config(base_data(body={"reasoning": True}), {})
        self.assertEqual(cfg_own.extra_body, {"reasoning": True, "think": True})

    def test_logging_view_masks_header_secrets_too(self):
        prof = {"model_url": "http://model.example/v1/chat",
                "model_name": "m", "api_key": "sk-live-secret",
                "extra_headers": {"X-Strata-Priority": "now",
                                  "Authorization": "Bearer abc",
                                  "X-Api-Key": "zzz"}}
        safe = str(redact(prof))
        self.assertNotIn("sk-live-secret", safe)
        self.assertNotIn("Bearer abc", safe)
        self.assertNotIn("zzz", safe)
        self.assertIn("now", safe)  # a priority lane is not a secret

    def test_profile_switch_changes_endpoint_not_code(self):
        options = {"profile": "path_b", "profiles": {
            "path_a": {"model_url": "http://a/v1/chat",
                       "model_name": "qwen3.5:4b"},
            "path_b": {"model_url": "http://b/v1/chat",
                       "model_name": "flash",
                       "extra_headers": {"X-Strata-Priority": "now"}}}}
        cfg = build_model_config(base_data(), options)
        self.assertEqual(cfg.model_name, "flash")
        self.assertEqual(cfg.extra_headers.get("X-Strata-Priority"), "now")
        active_profile(options)  # stays path_b; default rule below
        self.assertEqual(active_profile({}), "path_a")
        self.assertEqual(active_profile({"profile": "bogus"}), "path_a")

    def test_json_fields(self):
        table = [("", ({}, None)), (None, ({}, None)),
                 ('{"X-Strata-Priority": "now"}',
                  ({"X-Strata-Priority": "now"}, None)),
                 ("nonsense", (None, "bad_json")),
                 ("[1,2]", (None, "bad_json")),
                 ("x" * 5000, (None, "json_too_long"))]
        for text, want in table:
            self.assertEqual(parse_json_field(text), want)

    def test_form_build_and_rejects_bad_json(self):
        form = {"model_url": "http://h/v1/chat/", "model_name": " qwen ",
                "model_api_key": "sk-secret", "thinking": False,
                "extra_headers_json": '{"X-Strata-Priority":"now"}',
                "timeout_s": 12}
        prof = profile_from_form(form)
        self.assertEqual(prof["model_url"], "http://h/v1/chat")
        self.assertEqual(prof["api_key"], "sk-secret")
        self.assertFalse(prof["thinking"])
        with self.assertRaises(ValueError):
            profile_from_form({**form, "extra_body_json": "{"})

    def test_redact_hides_the_key_everywhere(self):
        prof = profile_from_form({"model_url": "http://h/v1",
                                  "model_name": "m",
                                  "model_api_key": "sk-live-secret"})
        entry_data = dict(base_data(model_api_key="sk-live-secret"))
        options = {"profiles": {"path_a": prof}}
        # the Config itself carries the key for the request...
        cfg = build_model_config(entry_data, options)
        self.assertEqual(cfg.api_key, "sk-live-secret")
        # ...but no logging view ever contains it
        merged = profiles_from_entry(entry_data, options)
        for profile in merged.values():
            self.assertNotIn("sk-live-secret", str(redact(profile)))

    def test_legacy_entry_without_profiles_still_builds(self):
        cfg = build_model_config(base_data(model_api_key="k"), {"profile": "path_b"})
        self.assertEqual(cfg.api_key, "k")  # both profiles inherit live fields


class FakeTransport:
    """The tiny transport contract ha_backend.house_read uses: scripted
    (status, body) outcome or a raised error; records every request."""

    def __init__(self, outcome=None):
        self.outcome = outcome if outcome is not None else (200, b'{"ok": 1}')
        self.seen = []  # (method, url, headers, timeout)

    async def request(self, method, url, *, headers=None, data=None,
                      timeout=None):
        self.seen.append((method, url, dict(headers or {}), timeout))
        if isinstance(self.outcome, BaseException):
            raise self.outcome
        return self.outcome


def house_backend(transport=None, url="http://chores.example:8123",
                  token="ro-example"):
    return _ha_backend.HaBackend(None, transport=transport, house_url=url,
                                 house_token=token)


def run_read(backend, kind):
    return asyncio.run(backend.house_read(kind))


class HouseReadTest(unittest.TestCase):
    """house_read builds the right bounded GET and never lets anything - a
    timeout, a dead host, a 401 or garbage json - escape as an exception."""

    def test_urls_headers_and_the_three_second_cap(self):
        for kind, path in (("chores", "/api/chores?status=all"),
                           ("plants", "/api/plants?status=all")):
            t = FakeTransport((200, b'{"status": "all", "count": 0}'))
            out = run_read(house_backend(t), kind)
            self.assertEqual(out, {"status": "all", "count": 0})
            self.assertEqual(t.seen, [("GET", "http://chores.example:8123" +
                                       path,
                                       {"Authorization": "Bearer ro-example"},
                                       _ha_backend.HOUSE_TIMEOUT_S)])
        # the spoken answer must not stall: a total timeout of 3 s or less
        self.assertLessEqual(_ha_backend.HOUSE_TIMEOUT_S, 3.0)

    def test_every_failure_becomes_none_not_an_exception(self):
        cases = [
            _errors.RequestTimeout("slow"),          # the 3 s cap firing
            _errors.TransportError("no route to host"),
            (401, b'{"error": "bad token"}'),        # read-only or not, a no
            (200, b"not json at all"),               # html error page etc.
            (200, b"[1, 2]"),                        # right port, wrong shape
        ]
        for outcome in cases:
            self.assertIsNone(run_read(house_backend(FakeTransport(outcome)),
                                       "chores"), outcome)

    def test_unset_config_short_circuits_before_any_request(self):
        cases = [
            {"house_url": None, "house_token": "ro-example"},
            {"house_url": "http://chores.example", "house_token": ""},
            {"house_url": None, "house_token": None},
        ]
        for kw in cases:
            t = FakeTransport()
            b = _ha_backend.HaBackend(None, transport=t, **kw)
            self.assertEqual(run_read(b, "plants"),
                             _housecontrol.HOUSE_NOT_SET_UP, kw)
            self.assertEqual(t.seen, [])  # unset feed means no request at all
        # configured, but this backend has no transport to speak on: same line
        b = _ha_backend.HaBackend(None, transport=None,
                                  house_url="http://chores.example",
                                  house_token="ro-example")
        self.assertEqual(run_read(b, "plants"), _housecontrol.HOUSE_NOT_SET_UP)


class ReplayCliTest(unittest.TestCase):
    """The replay CLI's repair round pinned down: a set read from a real path
    (it used to iterate a closed file object and die), the house file's
    private "_states" key not mistaken for an area, and --api-key-env keeping
    a model key off the command line. These guard fixes shipped untested;
    this is their coverage."""

    _replay = importlib.import_module("hs.replay")

    def test_load_set_from_a_path(self):
        with tempfile.NamedTemporaryFile(
                "w", suffix=".jsonl", delete=False, encoding="utf-8") as fh:
            fh.write('{"text": "turn on the kitchen"}\n'
                     "\n"
                     '{"id": "x2", "text": "dim it"}\n')
            path = fh.name
        try:
            rows = self._replay.load_set(path)
        finally:
            os.unlink(path)
        self.assertEqual([r["text"] for r in rows],
                         ["turn on the kitchen", "dim it"])
        self.assertTrue(all(isinstance(r.get("id"), str) and r["id"]
                            for r in rows))

    def test_house_file_private_keys_are_not_areas(self):
        house = {"Kitchen": {
            "lights": [["Kitchen lights", "light.kitchen"]],
            "climates": []},
            "_states": [{"entity_id": "light.kitchen", "state": "on",
                         "attributes": {"friendly_name": "Kitchen lights"}}]}
        backend = self._replay.CliBackend(house)
        areas = asyncio.run(backend.exposed())
        self.assertEqual(sorted(areas), ["Kitchen"])  # "_states" is not a room
        states = asyncio.run(backend.states(["light.kitchen", "light.gone"]))
        self.assertIn("light.kitchen", states)
        self.assertEqual(states["light.kitchen"]["state"], "on")
        self.assertNotIn("light.gone", states)

    def test_model_key_arrives_from_the_environment_only(self):
        os.environ["HELM_TEST_MODEL_KEY"] = "k-from-env"
        try:
            cfg = self._replay.config_from_args(types.SimpleNamespace(
                model_url="http://model.example/v1/chat", model_name="m",
                api_key_env="HELM_TEST_MODEL_KEY", header=[], body_json=None,
                no_thinking=True, timeout=30.0))
        finally:
            del os.environ["HELM_TEST_MODEL_KEY"]
        self.assertEqual(cfg.api_key, "k-from-env")
        cfg2 = self._replay.config_from_args(types.SimpleNamespace(
            model_url="http://model.example/v1/chat", model_name="m",
            api_key_env=None, header=[], body_json=None,
            no_thinking=True, timeout=30.0))
        self.assertIsNone(cfg2.api_key)


class ErrorLinesTest(unittest.TestCase):
    """errorlines: the one decision table, and the catalogue that feeds it."""

    def test_code_for_turn_table(self):
        ac = _agent_core
        el = _errorlines
        need = el.NEED_UPDATE_TEXT
        cases = [
            # main endpoint: the kind plus the fixed sentence it would speak
            ("main offline", dict(kind=ac.KIND_OFFLINE), "thinking_offline"),
            ("main timeout", dict(kind=ac.KIND_TIMEOUT), "took_too_long"),
            ("main error generic", dict(kind=ac.KIND_ERROR, text=ac.ERROR_TEXT), "try_again"),
            ("main error no text", dict(kind=ac.KIND_ERROR), "try_again"),
            ("main error empty", dict(kind=ac.KIND_ERROR, text=ac.EMPTY_TEXT), "did_not_catch_that"),
            ("main error gate blocked", dict(kind=ac.KIND_ERROR, text=ac.CONTROL_TEXT), "could_not_do_that"),
            ("answer keeps live words", dict(kind=ac.KIND_ANSWER, text="It is three."), None),
            ("escalate without a hub", dict(kind=ac.KIND_ESCALATE), None),
            ("no satellite device (text/REST call)", dict(kind=ac.KIND_ERROR, has_satellite=False), None),
            # hub side of the turn: result state, http status when one exists,
            # and the hub's own answer text for the exact-match line
            ("hub offline", dict(kind=ac.KIND_ESCALATE, escalate_state="offline"), "cannot_reach_helm"),
            ("hub busy 429", dict(kind=ac.KIND_ESCALATE, escalate_state="error", escalate_status=429), "too_busy"),
            ("hub refused the token", dict(kind=ac.KIND_ESCALATE, escalate_state="error", escalate_status=401), "try_again"),
            ("hub forgot the turn", dict(kind=ac.KIND_ESCALATE, escalate_state="error", escalate_status=404), "try_again"),
            ("hub gave up", dict(kind=ac.KIND_ESCALATE, escalate_state="error"), "try_again"),
            ("escalation budget over", dict(kind=ac.KIND_ESCALATE, escalate_state="timeout"), "answer_on_phone"),
            ("hub too old to answer", dict(kind=ac.KIND_ESCALATE, escalate_state="answer", escalate_text=need), "helm_needs_updating"),
            ("reworded hub line falls back to live words", dict(kind=ac.KIND_ESCALATE, escalate_state="answer", escalate_text=need + " please"), None),
            ("hub answered normally", dict(kind=ac.KIND_ESCALATE, escalate_state="answer", escalate_text="Four o'clock."), None),
        ]
        for name, kwargs, want in cases:
            with self.subTest(name):
                self.assertEqual(_errorlines.code_for_turn(**kwargs), want)

    def test_every_kind_that_ends_a_turn_has_a_line(self):
        # A new kind or hub state that ends a turn without an answer must get
        # a catalogue entry, or this fails. "answer" and escalate-without-hub
        # deliberately stay live speech.
        for kind in (_agent_core.KIND_OFFLINE, _agent_core.KIND_TIMEOUT,
                     _agent_core.KIND_ERROR):
            self.assertIsNotNone(_errorlines.code_for_turn(kind=kind), kind)
        for state in ("offline", "timeout", "error"):
            self.assertIsNotNone(
                _errorlines.code_for_turn(kind=_agent_core.KIND_ESCALATE,
                                          escalate_state=state), state)

    def test_every_fixed_sentence_is_covered_by_a_line(self):
        # Walks the module constants: a new OFFLINE_TEXT/ERROR_TEXT/... in
        # agent_core or hubapi without a "covers" entry fails here. The line's
        # own words may differ (they are re-recorded), so matching is by name.
        covered = set()
        for entry in _errorlines.lines().values():
            covered.update(entry.get("covers") or [])
        # GUARD_REPROMPT_TEXT is the one exempt constant: it is sent back to
        # the model inside the turn and never reaches a speaker.
        exempt = {"agent_core.GUARD_REPROMPT_TEXT"}
        for mod, mod_name in ((_agent_core, "agent_core"), (_hubapi, "hubapi")):
            for name in sorted(dir(mod)):
                qualified = "%s.%s" % (mod_name, name)
                if name.endswith("_TEXT") and qualified not in exempt:
                    self.assertIn(qualified, covered,
                                  "%s has no pre-recorded line" % qualified)

    def test_line_entries_are_speakable(self):
        for code, entry in _errorlines.lines().items():
            with self.subTest(code):
                self.assertLessEqual(len(entry["text"].split()), 12)
                self.assertTrue(entry["file"].endswith(".flac"))
                self.assertIsInstance(entry.get("situations"), list)
                self.assertTrue(entry["situations"])

    def test_files_and_words_come_from_the_json(self):
        el = _errorlines
        # the hub-unreachable line reuses the file that has played since before
        # this feature; everything else is helm_speaker_err_<code>.flac
        self.assertEqual(el.filename_for("cannot_reach_helm"),
                         "helm_speaker_offline_alba.flac")
        self.assertEqual(el.filename_for("try_again"),
                         "helm_speaker_err_try_again.flac")
        self.assertEqual(el.canonical_text("did_not_catch_that"),
                         _agent_core.EMPTY_TEXT)
        self.assertIsNone(el.canonical_text("no_such_code"))


if __name__ == "__main__":
    unittest.main()
