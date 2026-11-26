"""Glue tests that need no Home Assistant: profile build, switch and
secrecy. The HA-side acceptance (entry sets up hub-less; a processed turn
calls the right light service) runs in a scratch Home Assistant install,
because those names only exist with HA loaded.

Run with either plain python or the scratch venv: profiles.py imports only
agent_core and const.
"""

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


if __name__ == "__main__":
    unittest.main()
