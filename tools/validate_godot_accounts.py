#!/usr/bin/env python3
"""Validate account transport and two independent native Godot production scenes."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
PROJECT = ROOT / "examples/godot"
ACCOUNT_BINARY = ROOT / "target/debug/examples/account_server"
ARENA_BINARY = ROOT / "target/debug/campfire-server"
RUNTIME_ERRORS = re.compile(r"SCRIPT ERROR:|Parse Error:|ERROR:|Invalid call\.|Invalid access", re.I)
TOKEN_VALUE = re.compile(r'"token"\s*:\s*"|Bearer\s+[A-Za-z0-9_-]+')


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def sources() -> dict[str, str]:
    paths = [ROOT / "Cargo.toml", ROOT / "Cargo.lock", PROJECT / "project.godot", Path(__file__)]
    paths += list((ROOT / "crates").rglob("*.rs")) + list((ROOT / "crates").rglob("Cargo.toml"))
    for suffix in ("*.gd", "*.gd.uid", "*.tscn"):
        paths += list(PROJECT.rglob(suffix))
    return {str(path.relative_to(ROOT)): sha256(path) for path in sorted(set(paths))}


def source_digest(manifest: dict[str, str]) -> str:
    return hashlib.sha256(json.dumps(manifest, sort_keys=True).encode()).hexdigest()


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def wait_health(url: str, process: subprocess.Popen) -> None:
    deadline = time.monotonic() + 12
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"Test server exited: {process.returncode}")
        try:
            with urllib.request.urlopen(url + "/health", timeout=.3) as response:
                if response.status == 200 and json.load(response) == {"status": "ok"}:
                    return
        except OSError:
            pass
        time.sleep(.05)
    raise RuntimeError("Test server health timeout")


def stop(process: subprocess.Popen) -> None:
    # Only process objects returned by this runner's Popen are ever signalled.
    if process.poll() is None:
        process.send_signal(signal.SIGINT)
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.terminate()
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=3)


def read_json(path: Path) -> dict:
    return json.loads(path.read_text())


def command(godot: str, script: str, extra: list[str], log: Path, native: bool) -> list[str]:
    value = [godot, "--path", str(PROJECT), "--audio-driver", "Dummy", "--max-fps", "30", "--single-threaded-scene", "--log-file", str(log)]
    if native:
        value += ["--rendering-method", "gl_compatibility", "--resolution", "1280x800", "--position", "30,40" if extra[0] == "alice" else "100,110"]
    else:
        value += ["--headless"]
    return value + ["--script", script, "--", *extra]


def check_artifact_secrets(output: Path, credentials: list[str]) -> int:
    checked = 0
    for path in output.rglob("*"):
        if not path.is_file():
            continue
        data = path.read_bytes()
        if any(value.encode() in data for value in credentials):
            raise RuntimeError(f"Secret found in artifact: {path.name}")
        if path.suffix in {".json", ".log", ".md"} and TOKEN_VALUE.search(data.decode(errors="replace")):
            raise RuntimeError(f"Token-shaped value found in artifact: {path.name}")
        checked += 1
    return checked


def validate_native_reports(output: Path) -> dict:
    reports = [read_json(output / f"{role}.json") for role in ("alice", "bob")]
    for report in reports:
        if not report.get("success") or report.get("display_driver") == "headless" or not report.get("known_runtime_token_scan_passed"):
            raise RuntimeError(f"Native {report.get('role')} failed: {report.get('failures')}; errors={report.get('unexpected_errors')}")
        capture = output / f"{report['role']}-shared.png"
        if not capture.exists() or capture.stat().st_size < 1000:
            raise RuntimeError("Missing native renderer PNG")
    observations = [next(item for item in report["observations"] if item["label"] == "lost-reply-timeout-resumed") for report in reports]
    alice, bob = observations
    if alice["player_id"] == bob["player_id"] or len({item["room_id"] for item in observations}) != 1 or len({item["room_instance_id"] for item in observations}) != 1:
        raise RuntimeError("Native account identity or membership mismatch")
    if alice["public_state"] != bob["public_state"] or alice["public_state"] != {"players": 2, "total_collected": 4}:
        raise RuntimeError("Independent public state comparison failed")
    if alice["private_state"] != {"inventory": ["alice-starter-item", "wood", "key", "stone"]} or bob["private_state"] != {"inventory": ["bob-starter-item", "stone"]}:
        raise RuntimeError("Private inventories do not match authoritative collections")
    if len(alice["command_results"]) != 3 or len(bob["command_results"]) != 1:
        raise RuntimeError("Recipient command result separation failed")
    retry = reports[0]["retry_evidence"]
    if not retry["same_retained_command"] or retry["original_request_ids"] == retry["retry_request_ids"] or retry["command_seq"] != 2:
        raise RuntimeError("Same-command retry evidence is incomplete")
    if reports[0]["deliberately_discarded_socket_messages"].get("game.command.result", 0) < 2:
        raise RuntimeError("Lost-response tests did not discard real terminal replies")
    return {"distinct_native_pids": [report["pid"] for report in reports], "native_check_counts": {report["role"]: len(report["checks"]) for report in reports}, "same_room_id": alice["room_id"], "same_room_instance_id": alice["room_instance_id"], "matching_public_state": alice["public_state"], "private_inventory_counts": [len(item["private_state"]["inventory"]) for item in observations], "own_result_counts": [len(item["command_results"]) for item in observations], "captures": {role: {"path": f"{role}-shared.png", "sha256": sha256(output / f"{role}-shared.png")} for role in ("alice", "bob")}, "interaction_note": "Production Control signals; no OS click claimed. Real socket replies discarded. Separate retry fixture releases only transport request lock; natural six-second timeout/resume is also exercised."}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--godot", default=os.environ.get("GODOT", shutil.which("godot") or "/opt/homebrew/bin/godot"))
    parser.add_argument("--output", type=Path, required=True, help="New artifact directory; existing runs are never overwritten")
    parser.add_argument("--skip-build", action="store_true")
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    result: dict = {"success": False, "timestamp_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "invocation": sys.argv, "git_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(), "godot_version": subprocess.check_output([args.godot, "--version"], text=True).strip(), "runs": []}
    owned: list[subprocess.Popen] = []
    records: list[dict] = []
    handles = []
    generated = [secrets.token_urlsafe(32) for _ in range(3)]
    environment = os.environ.copy()
    environment.update(dict(zip(("CAMPFIRE_DEMO_ALICE_CREDENTIAL", "CAMPFIRE_DEMO_BOB_CREDENTIAL", "CAMPFIRE_TEST_INVALID_CREDENTIAL"), generated)))

    def launch(label: str, cmd: list[str], env: dict | None = None) -> subprocess.Popen:
        handle = (output / f"{label}.log").open("w")
        handles.append(handle)
        process = subprocess.Popen(cmd, cwd=ROOT, env=env, stdout=handle, stderr=subprocess.STDOUT)
        owned.append(process)
        record = {"label": label, "pid": process.pid, "command": cmd}
        records.append(record)
        return process

    try:
        if not args.skip_build:
            for label, cmd in (("build-account", ["cargo", "build", "-p", "campfire-server", "--no-default-features", "--example", "account_server", "--locked", "--offline"]), ("build-arena", ["cargo", "build", "-p", "campfire-server", "--locked", "--offline"])):
                process = launch(label, cmd)
                if process.wait(timeout=180):
                    raise RuntimeError(f"{label} failed; see log")
        manifest = sources()
        result["source_sha256_before"] = source_digest(manifest)
        result["source_manifest"] = manifest
        result["binary_sha256_before"] = {"godot": sha256(Path(args.godot).resolve()), "account_server": sha256(ACCOUNT_BINARY), "arena_server": sha256(ARENA_BINARY)}
        account_port, arena_port = free_port(), free_port()
        while account_port == arena_port:
            arena_port = free_port()
        account_url, arena_url = f"http://127.0.0.1:{account_port}", f"http://127.0.0.1:{arena_port}"
        environment["CAMPFIRE_DEMO_BIND"] = f"127.0.0.1:{account_port}"
        environment["CAMPFIRE_TEST_ACCOUNT_URL"] = account_url
        arena_cmd = [str(ARENA_BINARY), "--bind", f"127.0.0.1:{arena_port}", "--game", "arena", "--game", "arena-b", "--capacity", "16", "--guest-burst", "100", "--guest-rate-per-second", "100"]
        arena = launch("arena-server", arena_cmd)
        wait_health(arena_url, arena)
        account = launch("account-transport-server", [str(ACCOUNT_BINARY)], environment)
        wait_health(account_url, account)
        print("Running real guest/account transport regression", flush=True)
        cmd = command(args.godot, "res://tests/client_unit_transport.gd", [arena_url], output / "transport-engine.log", False)
        transport = launch("transport", cmd, environment)
        if transport.wait(timeout=90):
            raise RuntimeError("Real transport suite failed; see transport.log")
        if "PASS" not in (output / "transport.log").read_text():
            raise RuntimeError("Transport suite did not emit PASS")
        result["transport_success"] = True
        stop(account)
        # A fresh runtime prevents transport test token rotation from affecting GUI accounts.
        account = launch("account-native-server", [str(ACCOUNT_BINARY)], environment)
        wait_health(account_url, account)
        print("Running two independent native production scenes", flush=True)
        with tempfile.TemporaryDirectory(prefix="campfire-account-native-") as temporary:
            native = []
            for role in ("alice", "bob"):
                cmd = command(args.godot, "res://tests/integration_accounts.gd", [role, temporary, str(output), account_url, arena_url], output / f"{role}-engine.log", True)
                native.append(launch(role, cmd, environment))
            deadline = time.monotonic() + 100
            while any(process.poll() is None for process in native):
                if time.monotonic() > deadline:
                    raise RuntimeError("Native account scenario timed out")
                for process in native:
                    if process.poll() not in (None, 0):
                        raise RuntimeError(f"Native Godot process exited {process.returncode}")
                time.sleep(.1)
            result["native"] = validate_native_reports(output)
        for process in owned:
            stop(process)
        for handle in handles:
            handle.flush()
        for path in output.glob("*.log"):
            if RUNTIME_ERRORS.search(path.read_text(errors="replace")):
                raise RuntimeError(f"Runtime/parse error in {path.name}")
        result["artifact_secret_scan_files"] = check_artifact_secrets(output, generated)
        result["source_sha256_after"] = source_digest(sources())
        result["binary_sha256_after"] = {"godot": sha256(Path(args.godot).resolve()), "account_server": sha256(ACCOUNT_BINARY), "arena_server": sha256(ARENA_BINARY)}
        if result["source_sha256_before"] != result["source_sha256_after"] or result["binary_sha256_before"] != result["binary_sha256_after"]:
            raise RuntimeError("Source or binary changed during validation")
        result["success"] = True
        print("PASS: two native Godot accounts, isolated private state, same-command retry and real timeout/resume", flush=True)
    except Exception as error:
        result["failure"] = str(error)
        print(f"FAIL: {error}", file=sys.stderr, flush=True)
    finally:
        for process in owned:
            stop(process)
        for handle in handles:
            handle.close()
        for process, record in zip(owned, records):
            record["exit_code"] = process.returncode
            record["alive_after_cleanup"] = process.poll() is None
        result["runs"] = records
        result["owned_process_cleanup_complete"] = all(not item["alive_after_cleanup"] for item in records)
        try:
            check_artifact_secrets(output, generated)
        except RuntimeError as error:
            print(f"FAIL: {error}", file=sys.stderr)
            result["success"] = False
            result["failure"] = str(error)
        serialized = json.dumps(result, indent=2, ensure_ascii=False) + "\n"
        if any(value in serialized for value in generated):
            result = {"success": False, "failure": "Secret detected before summary serialization", "owned_process_cleanup_complete": result["owned_process_cleanup_complete"]}
            serialized = json.dumps(result, indent=2) + "\n"
        (output / "summary.json").write_text(serialized)
        generated.clear()
    return 0 if result["success"] else 1


if __name__ == "__main__":
    sys.exit(main())
