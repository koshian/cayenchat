#!/usr/bin/env python3
"""Checks that gui_session.py keeps to its session directory and away from
the user's passwords, without starting a display:

    python3 -m unittest discover -s scripts/e2e -p 'test_*.py'
"""
import argparse, json, os, subprocess, sys, tempfile, unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import gui_session  # noqa: E402

SCRIPT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "gui_session.py")


class SessionDirectory(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.directory = self.scratch.name

    def tearDown(self):
        self.scratch.cleanup()

    def write(self, *parts):
        path = os.path.join(self.directory, *parts)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as file:
            file.write("keep")
        return path

    def test_start_refuses_a_directory_it_did_not_make(self):
        notes = self.write("notes.txt")
        result = subprocess.run([sys.executable, SCRIPT, "--session", self.directory, "start",
                                 "--app", sys.executable], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not made by gui_session.py", result.stderr)
        self.assertTrue(os.path.exists(notes))
        self.assertEqual(os.listdir(self.directory), ["notes.txt"])

    def test_an_empty_directory_becomes_a_session(self):
        gui_session.prepare_directory(self.directory, keep=False)
        self.assertTrue(os.path.exists(os.path.join(self.directory, gui_session.MARKER)))

    def test_clearing_a_session_removes_only_its_own_entries(self):
        gui_session.prepare_directory(self.directory, keep=False)
        shot = self.write("shots", "01_started.png")
        settings = self.write("home", "config", "CayenChat", "settings.json")
        notes = self.write("notes.txt")
        gui_session.prepare_directory(self.directory, keep=False)
        self.assertFalse(os.path.exists(shot))
        self.assertFalse(os.path.exists(settings))
        self.assertTrue(os.path.exists(notes))

    def test_keep_leaves_the_session_as_it_is(self):
        gui_session.prepare_directory(self.directory, keep=False)
        settings = self.write("home", "config", "CayenChat", "settings.json")
        gui_session.prepare_directory(self.directory, keep=True)
        self.assertTrue(os.path.exists(settings))


class Passwords(unittest.TestCase):
    def test_copied_settings_keep_passwords_in_the_session(self):
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as file:
            json.dump({"version": 15, "credential_backend": "system",
                       "servers": [{"id": "a", "host": "irc.example", "port": 6697}]}, file)
        try:
            args = argparse.Namespace(settings=file.name)
            settings = json.loads(gui_session.settings_json(args))
        finally:
            os.remove(file.name)
        self.assertEqual(settings["credential_backend"], "local_file")
        self.assertEqual(settings["servers"][0]["host"], "irc.example")

    def test_the_app_cannot_reach_the_desktop_session_bus(self):
        with tempfile.TemporaryDirectory() as directory:
            base = {"DBUS_SESSION_BUS_ADDRESS": "unix:path=/run/user/1000/bus",
                    "XDG_RUNTIME_DIR": "/run/user/1000", "HOME": "/home/user",
                    "WAYLAND_DISPLAY": "wayland-0", "PATH": "/usr/bin"}
            extra = ["DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus", "HOME=/home/user"]
            env = gui_session.app_env(base, directory, ":98", extra)
            self.assertNotIn("DBUS_SESSION_BUS_ADDRESS", env)
            self.assertNotIn("WAYLAND_DISPLAY", env)
            for key in ("HOME", "XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "XDG_DATA_HOME",
                        "XDG_CACHE_HOME", "CAYENCHAT_TEST_DIR"):
                self.assertTrue(env[key].startswith(directory + os.sep), key)
            self.assertEqual(env["PATH"], "/usr/bin")


if __name__ == "__main__":
    unittest.main()
