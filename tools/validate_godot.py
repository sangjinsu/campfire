#!/usr/bin/env python3
"""Launch real Godot clients against the actual Campfire server (stdlib only)."""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
PROJECT = ROOT / "examples/godot"
RUNTIME_ERRORS = re.compile(r"SCRIPT ERROR:|Parse Error:|ERROR:|Invalid call\.|Invalid access", re.I)


def wait_for(check, timeout: float, label: str, processes=()):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        for process in processes:
            if process.poll() is not None:
                raise RuntimeError(f"{label}: process exited {process.returncode}")
        time.sleep(0.05)
    raise RuntimeError(f"Timeout: {label}")


def read_json(path: Path):
    try:
        return json.loads(path.read_text())
    except (FileNotFoundError, json.JSONDecodeError):
        return None


def source_hash():
    digest = hashlib.sha256()
    paths = [ROOT / "Cargo.toml", ROOT / "Cargo.lock"]
    paths += list((ROOT / "crates").rglob("*.rs"))
    paths += list((ROOT / "crates").rglob("Cargo.toml"))
    paths += list((ROOT / "crates/campfire-server/src/dashboard/assets").glob("*"))
    paths += list((ROOT / "tools").glob("*dashboard*"))
    paths += list((ROOT / "tools/tests").glob("dashboard*"))
    paths += list(PROJECT.rglob("*.gd")) + list(PROJECT.rglob("*.gd.uid")) + list(PROJECT.rglob("*.tscn")) + [PROJECT / "project.godot", Path(__file__)]
    for path in sorted(path for path in paths if path.is_file()):
        digest.update(str(path.relative_to(ROOT)).encode() + b"\0" + path.read_bytes())
    return digest.hexdigest()


def terminate(processes):
    for process in processes:
        if process.poll() is None:
            process.terminate()
    for process in processes:
        try:
            process.wait(timeout=4)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=4)


def engine_command(godot, script, extra, graphical=False):
    command = [godot, "--path", str(PROJECT), "--audio-driver", "Dummy", "--max-fps", "30", "--single-threaded-scene"]
    if not graphical:
        command += ["--headless"]
    command += ["--script", script, "--", *map(str, extra)]
    return command


def validate_soak(godot, server_url, count, duration, out, temporary):
    work = temporary / f"clients-{count}"
    work.mkdir()
    config = {"server": server_url, "count": count, "duration": duration}
    config_path = work / "config.json"
    config_path.write_text(json.dumps(config))
    processes, handles = [], []
    commands = []
    started = time.monotonic()
    try:
        for index in range(count):
            command = engine_command(godot, "res://tests/bot.gd", [config_path, work, index])
            commands.append(command)
            handle = (out / f"bot-{index:02}.log").open("w")
            handles.append(handle)
            processes.append(subprocess.Popen(command, cwd=ROOT, stdout=handle, stderr=subprocess.STDOUT))
            # Explicit validation-only server burst allows these bounded starts.
            time.sleep(0.10)
        wait_for(lambda: all(read_json(work / f"ready-{i}.json") for i in range(count)), 35, f"{count} Godot clients ready", processes)
        (work / "start.json").write_text("{}")
        scenario_started = time.monotonic()
        wait_for(lambda: time.monotonic() - scenario_started >= duration, duration + 2, "measured movement", processes)
        (work / "finish.json").write_text("{}")
        for process in processes:
            process.wait(timeout=15)
        reports = [read_json(work / f"report-{i}.json") for i in range(count)]
        if not all(reports):
            raise RuntimeError("Missing Godot client report")
        for index, (process, report) in enumerate(zip(processes, reports)):
            if process.returncode or not report["success"]:
                raise RuntimeError(f"Godot bot {index}: {report['failure']}")
            if not report["monotonic"] or report["max_members"] != count or report["last_processed_input_seq"] < 100 or report["own_position_span"] < 1.0 or report["snapshot_count"] < duration * 8:
                raise RuntimeError(f"Godot bot {index}: movement/state/ack coverage insufficient")
            if report["duration_seconds"] < duration - 0.5:
                raise RuntimeError(f"Godot bot {index}: insufficient measured duration")
        reconnect = reports[-1]
        if not reconnect["reconnect_done"] or not reconnect["reconnect_resumed"]:
            raise RuntimeError("Reconnect did not resume the same Godot client")
        ids = {report["guest_id"] for report in reports}
        if len(ids) != count or len({r["room_id"] for r in reports}) != 1 or len({r["room_instance_id"] for r in reports}) != 1:
            raise RuntimeError("Guest/room identity mismatch")
        # Compare full public state at exactly the same server snapshot sequence.
        states_by_client = [{int(o["snapshot_seq"]): o for o in r["observations"] if len(o["state"]["players"]) == count} for r in reports]
        common = set(states_by_client[0])
        for states in states_by_client[1:]:
            common &= set(states)
        if len(common) < 8:
            raise RuntimeError(f"Only {len(common)} common full-state observations")
        shared = []
        for seq in sorted(common):
            observations = [states[seq] for states in states_by_client]
            canonical = [json.dumps(o["state"], sort_keys=True, separators=(",", ":")) for o in observations]
            if len(set(canonical)) != 1 or len({o["server_tick"] for o in observations}) != 1:
                raise RuntimeError(f"Shared state differs at sequence {seq}")
            shared.append({"snapshot_seq": seq, "server_tick": observations[0]["server_tick"], "state_sha256": hashlib.sha256(canonical[0].encode()).hexdigest(), "state": observations[0]["state"]})
        for report in reports[:-1]:
            disconnected = report["remote_disconnect"]
            if len(disconnected) < 3 or not report["saw_remote_return"]:
                raise RuntimeError("Remote clients did not observe disconnect/resume transition")
            if max(p["x"] for p in disconnected) - min(p["x"] for p in disconnected) > 0.001 or max(p["y"] for p in disconnected) - min(p["y"] for p in disconnected) > 0.001:
                raise RuntimeError("Disconnected remote player continued moving")
        for guest in ids:
            positions = [p for observation in shared for p in observation["state"]["players"] if p["id"] == guest]
            if max(p["x"] for p in positions) - min(p["x"] for p in positions) < 1.0 and max(p["y"] for p in positions) - min(p["y"] for p in positions) < 1.0:
                raise RuntimeError("Remote authoritative position did not move")
        for index, report in enumerate(reports):
            gaps = sorted(report.pop("snapshot_gaps_ms"))
            report["snapshot_gap_ms_p95"] = gaps[max(0, math.ceil(len(gaps) * .95) - 1)] if gaps else None
            report["snapshot_gap_ms_max"] = max(gaps) if gaps else None
            report.pop("observations")
            (out / f"bot-{index:02}.json").write_text(json.dumps(report, indent=2) + "\n")
        result = {"success": True, "client_engine": "Godot WebSocketPeer via production CampfireClient", "count": count, "measured_seconds": time.monotonic() - scenario_started, "total_seconds": time.monotonic() - started, "config": config, "commands": commands, "common_state_checks": shared, "clients": reports}
        (out / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        return result
    finally:
        terminate(processes)
        for handle in handles:
            handle.close()


def run_integration(godot, server_url, temporary, out, graphical):
    report_path = temporary / "integration.json"
    capture = out / "ui-live.png"
    command = engine_command(godot, "res://tests/integration_ui.gd", [server_url, report_path, capture], graphical)
    with (out / "integration.log").open("w") as handle:
        process = subprocess.Popen(command, cwd=ROOT, stdout=handle, stderr=subprocess.STDOUT)
        try:
            process.wait(timeout=55)
        finally:
            terminate([process])
    report = read_json(report_path)
    if report:
        report["command"] = command
        report["graphical"] = graphical
        (out / "integration.json").write_text(json.dumps(report, indent=2) + "\n")
    if process.returncode or not report or not report.get("success"):
        raise RuntimeError(f"Godot UI/negative integration failed; see {out / 'integration.log'}; report={report}")
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--godot", default=os.environ.get("GODOT", shutil.which("godot") or "/opt/homebrew/bin/godot"))
    parser.add_argument("--counts", nargs="+", type=int, default=[8, 16], choices=[8, 16])
    parser.add_argument("--duration", type=float, default=20.0)
    parser.add_argument("--output", type=Path, default=ROOT / "artifacts/godot-validation")
    parser.add_argument("--skip-build", action="store_true")
    parser.add_argument("--skip-ui", action="store_true")
    parser.add_argument("--ui-only", action="store_true")
    parser.add_argument("--headless-ui", action="store_true")
    args = parser.parse_args()
    if not 15 <= args.duration <= 120:
        parser.error("duration must be 15..120 seconds")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    all_processes = []
    server_handle = None
    result = {"success": False, "source_sha256": source_hash(), "git_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(), "git_status": subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT, text=True).splitlines(), "godot_version": subprocess.check_output([args.godot, "--version"], text=True).strip(), "invocation": sys.argv, "timestamp_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}
    try:
        if not args.skip_build:
            subprocess.run(["cargo", "build", "-p", "campfire-server", "--locked", "--offline"], cwd=ROOT, check=True)
        result["server_binary_sha256"] = hashlib.sha256((ROOT / "target/debug/campfire-server").read_bytes()).hexdigest()
        result["rust_version"] = subprocess.check_output(["rustc", "--version"], cwd=ROOT, text=True).strip()
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        server_url = f"http://127.0.0.1:{port}"
        server_command = [str(ROOT / "target/debug/campfire-server"), "--bind", f"127.0.0.1:{port}", "--game", "arena", "--game", "arena-b", "--capacity", "16", "--reconnect-grace-ms", "2000", "--guest-burst", "100", "--guest-rate-per-second", "100"]
        result["server_command"] = server_command
        result["configuration_note"] = "Validation-only guest burst/rate100 avoids default issuer burst10; reconnect grace shortened from30000 to2000ms. Other server timing/transport defaults retained."
        server_handle = (output / "server.log").open("w")
        server = subprocess.Popen(server_command, cwd=ROOT, stdout=server_handle, stderr=subprocess.STDOUT)
        all_processes.append(server)
        def healthy():
            try:
                with urllib.request.urlopen(server_url + "/health", timeout=.5) as response:
                    return response.status == 200 and json.load(response) == {"status": "ok"}
            except OSError:
                return False
        wait_for(healthy, 10, "server health", [server])
        result["health"] = {"status": "ok"}
        with tempfile.TemporaryDirectory(prefix="campfire-godot-") as directory:
            temporary = Path(directory)
            result["scenarios"] = []
            for count in ([] if args.ui_only else args.counts):
                out = output / f"clients-{count}"
                out.mkdir(exist_ok=True)
                scenario = validate_soak(args.godot, server_url, count, args.duration, out, temporary)
                result["scenarios"].append({"count": count, "success": scenario["success"], "common_state_checks": len(scenario["common_state_checks"]), "measured_seconds": scenario["measured_seconds"]})
                print(f"PASS: {count} actual Godot clients, {scenario['measured_seconds']:.2f}s, {len(scenario['common_state_checks'])} matching public states", flush=True)
            if not args.skip_ui:
                result["integration"] = run_integration(args.godot, server_url, temporary, output, not args.headless_ui)
        terminate(all_processes)
        for path in output.rglob("*.log"):
            content = path.read_text(errors="replace")
            if RUNTIME_ERRORS.search(content):
                raise RuntimeError(f"Runtime/parse error in {path}")
            if re.search(r'"token"\s*:\s*"|Bearer\s+[A-Za-z0-9_-]+', content):
                raise RuntimeError(f"Credential unexpectedly logged in {path}")
        if source_hash() != result["source_sha256"]:
            raise RuntimeError("Validation source changed during run")
        result["success"] = True
    except Exception as error:
        result["failure"] = str(error)
        print(f"FAIL: {error}", file=sys.stderr)
    finally:
        terminate(all_processes)
        if server_handle:
            server_handle.close()
        (output / "summary.json").write_text(json.dumps(result, indent=2) + "\n")
    return 0 if result["success"] else 1


if __name__ == "__main__":
    sys.exit(main())
