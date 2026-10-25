"""Regression tests for validation runner resource and executable boundaries."""
from __future__ import annotations

import os
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import validate_dashboard as dashboard
import validate_godot as godot_runner
import validate_godot_accounts as account_runner
import validation_paths


class ValidationPathsTests(unittest.TestCase):
    def test_target_dir_precedence_and_binary_layout(self):
        with tempfile.TemporaryDirectory() as directory:
            explicit = Path(directory) / "explicit"
            inherited = Path(directory) / "inherited"
            with mock.patch.dict(os.environ, {"CARGO_TARGET_DIR": str(inherited)}):
                self.assertEqual(validation_paths.resolve_target_dir(str(explicit)), explicit.resolve())
                self.assertEqual(validation_paths.resolve_target_dir(None), inherited.resolve())
            self.assertEqual(validation_paths.server_binary(explicit), explicit / "debug/campfire-server")
            self.assertEqual(validation_paths.account_binary(explicit), explicit / "debug/examples/account_server")

    def test_executable_precedence_and_missing_tool(self):
        with tempfile.TemporaryDirectory() as directory:
            first = Path(directory) / "first"
            second = Path(directory) / "second"
            for path in (first, second):
                path.write_text("#!/bin/sh\n")
                path.chmod(0o755)
            with mock.patch.dict(os.environ, {"GODOT": str(first)}, clear=False):
                self.assertEqual(validation_paths.resolve_executable(str(second), "GODOT", "godot"), second.resolve())
                self.assertEqual(validation_paths.resolve_executable(None, "GODOT", "godot"), first.resolve())
            with mock.patch.dict(os.environ, {"PATH": directory, "GODOT": ""}, clear=False):
                with self.assertRaisesRegex(FileNotFoundError, "godot"):
                    validation_paths.resolve_executable(None, "GODOT", "godot")


class BrowserIsolationTests(unittest.TestCase):
    def test_two_runners_override_inherited_profile_and_stop_with_owned_env(self):
        with tempfile.TemporaryDirectory() as directory:
            parent = Path(directory)
            inherited_profile = parent / "existing-user-profile"
            inherited_profile.mkdir()
            inherited_state = parent / "existing-user-state"
            inherited_state.write_text("untouched")
            with mock.patch.dict(os.environ, {"CHROMIUM_PROFILE": str(inherited_profile), "BROWSE_STATE_FILE": str(inherited_state)}):
                first = dashboard.Runner(parent / "first", "/godot", "/browse", parent / "target")
                second = dashboard.Runner(parent / "second", "/godot", "/browse", parent / "target")
            try:
                state_a = first.browse_env["BROWSE_STATE_FILE"]
                profile_a = first.browse_env["CHROMIUM_PROFILE"]
                self.assertNotEqual(state_a, second.browse_env["BROWSE_STATE_FILE"])
                self.assertNotEqual(profile_a, second.browse_env["CHROMIUM_PROFILE"])
                self.assertEqual(Path(state_a).parent, Path(profile_a).parent)
                self.assertNotEqual(profile_a, str(inherited_profile))
                calls = []

                def fake_run(command, **kwargs):
                    calls.append((command, kwargs["env"].copy()))
                    return mock.Mock(returncode=0, stdout="ok", stderr="")

                with mock.patch.object(dashboard.subprocess, "run", side_effect=fake_run):
                    first.browse("goto", "http://127.0.0.1:1/admin")
                    first.close()
                self.assertEqual([call[0][-1] for call in calls], ["http://127.0.0.1:1/admin", "stop"])
                self.assertTrue(all(env["BROWSE_STATE_FILE"] == state_a and env["CHROMIUM_PROFILE"] == profile_a for _, env in calls))
                self.assertTrue(all(command[0] == "/browse" for command, _ in calls))
            finally:
                owned_roots = (first.browse_dir, second.browse_dir)
                second.close()
                first.cleanup()
                second.cleanup()
                self.assertTrue(all(not path.exists() for path in owned_roots))
                self.assertTrue(inherited_profile.is_dir())
                self.assertEqual(inherited_state.read_text(), "untouched")

    def test_target_binary_used_by_server_and_hash(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / "target"
            binary = validation_paths.server_binary(target)
            binary.parent.mkdir(parents=True)
            binary.write_bytes(b"selected-server")
            runner = dashboard.Runner(root / "output", "/godot", "/browse", target)
            try:
                with mock.patch.object(account_runner, "free_port", return_value=13579), mock.patch.object(account_runner, "wait_health"), mock.patch.object(runner, "launch", return_value=mock.Mock()) as launch:
                    runner.server()
                self.assertEqual(Path(launch.call_args.args[1][0]), binary)
                self.assertEqual(dashboard.sha(runner.binary), dashboard.sha(binary))
            finally:
                runner.close()
                runner.cleanup()

    def test_stop_timeout_keeps_profile_and_still_cleans_owned_processes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            runner = dashboard.Runner(root / "output", "/godot", "/browse", root / "target")
            process = mock.Mock()
            process.poll.return_value = 0
            runner.processes.append(process)
            handle = mock.Mock()
            handle.closed = False
            handle.close.side_effect = lambda: setattr(handle, "closed", True)
            runner.handles.append(handle)
            runner.browser_started = True
            with mock.patch.object(dashboard.subprocess, "run", side_effect=subprocess.TimeoutExpired(["/browse", "stop"], 45)), mock.patch.object(account_runner, "stop") as stop:
                runner.close()
            stop.assert_called_once_with(process)
            self.assertTrue(handle.closed)
            self.assertFalse(runner.closed)
            self.assertTrue(runner.browser_started)
            self.assertFalse(runner.cleanup())
            self.assertTrue(runner.browse_dir.exists())
            with mock.patch.object(dashboard.subprocess, "run", return_value=mock.Mock(returncode=0, stdout="ok", stderr="")):
                runner.close()
            self.assertTrue(runner.closed)
            self.assertTrue(runner.cleanup())
            self.assertFalse(runner.browse_dir.exists())

    def _assert_failed_stop_summary(self, failure_mode):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / "target"
            binary = validation_paths.server_binary(target)
            binary.parent.mkdir(parents=True)
            binary.write_bytes(b"browser-server")
            godot = root / "godot"
            browse = root / "browse"
            for executable in (godot, browse):
                executable.write_text("#!/bin/sh\n")
                executable.chmod(0o755)
            output = root / "result"
            argv = ["validate_dashboard.py", "--suite", "browser", "--output", str(output), "--target-dir", str(target), "--godot", str(godot), "--browse", str(browse)]
            original_mkdtemp = tempfile.mkdtemp

            def browse_run(command, **kwargs):
                if command[-1] == "stop":
                    if failure_mode == "timeout":
                        raise subprocess.TimeoutExpired(command, 45)
                    return mock.Mock(returncode=1, stdout="", stderr="] ERROR: stop failed")
                return mock.Mock(returncode=0, stdout="ok", stderr="")

            def browser_suite(runner):
                runner.browse("viewport", "390x844")
                return {"checks": ["mock browser activity"]}

            with mock.patch.object(sys, "argv", argv), mock.patch.object(dashboard.tempfile, "mkdtemp", side_effect=lambda **kwargs: original_mkdtemp(dir=root, **kwargs)), mock.patch.object(dashboard.subprocess, "run", side_effect=browse_run), mock.patch.object(dashboard.subprocess, "check_output", return_value="test-commit\n"), mock.patch.object(dashboard, "browser_suite", side_effect=browser_suite):
                self.assertEqual(dashboard.main(), 1)
            summary = json.loads((output / "summary.json").read_text())
            self.assertFalse(summary["success"])
            self.assertFalse(summary["browser_stop_confirmed"])
            self.assertFalse(summary["browser_state_cleanup_complete"])
            self.assertTrue(Path(summary["gstack_state_directory"]).exists())
            self.assertIn("stop", summary["failure"])

    def test_abnormal_stop_records_failed_summary_and_preserves_profile(self):
        self._assert_failed_stop_summary("nonzero")

    def test_stop_timeout_records_failed_summary_and_preserves_profile(self):
        self._assert_failed_stop_summary("timeout")


class NestedRunnerTests(unittest.TestCase):
    def test_guest_and_account_binaries_follow_target_dir(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "build"
            self.assertEqual(godot_runner.server_binary(target), target / "debug/campfire-server")
            self.assertEqual(account_runner.server_binary(target), target / "debug/campfire-server")
            self.assertEqual(account_runner.account_binary(target), target / "debug/examples/account_server")

    def test_load_suite_does_not_require_browse_or_account_binary(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / "target"
            binary = validation_paths.server_binary(target)
            binary.parent.mkdir(parents=True)
            binary.write_bytes(b"selected-load-binary")
            godot = root / "godot"
            godot.write_text("#!/bin/sh\n")
            godot.chmod(0o755)
            output = root / "result"
            argv = ["validate_dashboard.py", "--suite", "load", "--output", str(output), "--target-dir", str(target), "--godot", str(godot)]
            with mock.patch.object(sys, "argv", argv), mock.patch.dict(os.environ, {"PATH": "", "BROWSE_BIN": ""}), mock.patch.object(dashboard.subprocess, "check_output", return_value="test-commit\n"), mock.patch.object(dashboard, "load_suite", return_value=[{"success": True}]):
                self.assertEqual(dashboard.main(), 0)
            summary = json.loads((output / "summary.json").read_text())
            self.assertEqual(summary["binary_sha256_before"]["server"], hashlib.sha256(b"selected-load-binary").hexdigest())
            self.assertEqual(summary["binary_sha256_before"], summary["binary_sha256_after"])
            self.assertNotIn("account_server", summary["binary_sha256_before"])
            self.assertNotIn("browse", summary["binary_sha256_before"])
            self.assertEqual(summary["load"], [{"success": True}])


if __name__ == "__main__":
    unittest.main()
