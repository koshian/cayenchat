#!/usr/bin/env python3
"""Checks the settings demo.py writes, without a display or a server:

    python3 -m unittest discover -s scripts/screenshots -p 'test_*.py'
"""
import argparse, json, os, shutil, sys, tempfile, time, unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import demo  # noqa: E402


class Settings(unittest.TestCase):
    def test_previews_and_compact_urls_are_on_and_avatars_on_for_the_server(self):
        for language, script in demo.SCRIPTS.items():
            settings = demo.settings(script, 1234, language, "light")
            for key in ("image_previews", "compact_urls"):
                self.assertIs(settings["appearance"].get(key), True, f"{language}: {key}")
            self.assertIs(settings["servers"][0].get("user_avatars"), True, language)

    def test_the_toolbar_is_shown_unless_asked_otherwise(self):
        for language, script in demo.SCRIPTS.items():
            self.assertIs(demo.settings(script, 1234, language, "light")["show_toolbar"], True)
            self.assertIs(demo.settings(script, 1234, language, "light", False)["show_toolbar"],
                          False)

    def test_every_channel_is_joined_and_server_is_local(self):
        for language, script in demo.SCRIPTS.items():
            server = demo.settings(script, 1234, language, "dark")["servers"][0]
            self.assertEqual(server["host"], "127.0.0.1")
            self.assertEqual(server["channels"].split(","),
                             [channel["name"] for channel in script["channels"]])

    def test_the_shown_channel_has_a_me_line(self):
        for language, script in demo.SCRIPTS.items():
            lines = script["channels"][0]["lines"]
            self.assertTrue(any(text.startswith("\x01ACTION ") and text.endswith("\x01")
                                for _, text in lines), language)


class Build(unittest.TestCase):
    """resolve_app takes only the file identical to the demo build made now."""

    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.built = os.path.join(self.scratch.name, "built")
        with open(self.built, "wb") as file:
            file.write(b"demo build of the current sources")
        self.build = mock.patch.object(demo, "build_demo", return_value=self.built)
        self.build.start()

    def tearDown(self):
        self.build.stop()
        self.scratch.cleanup()

    def resolve(self, app):
        return demo.resolve_app(argparse.Namespace(app=app))

    def other(self, content):
        path = os.path.join(self.scratch.name, "other")
        with open(path, "wb") as file:
            file.write(content)
        return path

    def test_no_app_runs_a_staged_copy_of_the_fresh_build(self):
        staged = self.resolve(None)
        self.assertNotEqual(staged, self.built)
        self.assertEqual(demo.sha256(staged), demo.sha256(self.built))

    def test_a_copy_of_the_build_is_accepted_but_the_staged_file_runs(self):
        copy = os.path.join(self.scratch.name, "copy")
        shutil.copyfile(self.built, copy)
        self.assertNotEqual(self.resolve(copy), copy)

    def test_the_staged_copy_has_the_current_catalogs_beside_it(self):
        staged = self.resolve(None)
        for name in ("en.json", "ja.json"):
            with open(os.path.join(demo.ROOT, "locales", name), "rb") as file:
                current = file.read()
            with open(os.path.join(os.path.dirname(staged), "locales", name), "rb") as file:
                self.assertEqual(file.read(), current)

    def test_an_old_build_with_a_new_time_is_refused(self):
        old = self.other(b"demo build of older sources")
        os.utime(old, (time.time() + 3600,) * 2)
        with self.assertRaises(SystemExit):
            self.resolve(old)

    def test_a_build_with_other_features_is_refused(self):
        with self.assertRaises(SystemExit):
            self.resolve(self.other(b"test-build,preview-fixture"))

    def test_a_missing_binary_is_refused(self):
        with self.assertRaises(SystemExit):
            self.resolve(os.path.join(self.scratch.name, "missing"))


class Artifact(unittest.TestCase):
    def test_the_executable_is_the_one_cargo_reports(self):
        messages = "\n".join(json.dumps(message) for message in [
            {"reason": "compiler-artifact", "executable": "/dep/tool",
             "target": {"name": "tool", "kind": ["bin"]}},
            {"reason": "compiler-artifact", "executable": "/elsewhere/release/cayenchat",
             "target": {"name": "cayenchat", "kind": ["bin"]}},
            {"reason": "build-finished", "success": True},
        ]) + "\nnot json"
        self.assertEqual(demo.executable_of(messages), "/elsewhere/release/cayenchat")

    def test_no_artifact_is_none(self):
        self.assertIsNone(demo.executable_of('{"reason": "build-finished"}'))


if __name__ == "__main__":
    unittest.main()
