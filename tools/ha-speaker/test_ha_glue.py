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


if __name__ == "__main__":
    unittest.main()
