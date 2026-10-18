#!/usr/bin/env python3
"""Reproduce loopback dashboard, real Godot load and account regression evidence.

Only stdlib and the existing gstack browse / Godot installations are used.
Never builds binaries, modifies an existing server, or signals unowned processes.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import secrets
import shutil
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

import validate_godot as guest
import validate_godot_accounts as accounts

ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / "target/debug/campfire-server"
ACCOUNT = ROOT / "target/debug/examples/account_server"
BROWSE = Path.home() / ".claude/skills/gstack/browse/dist/browse"


def sha(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def source_manifest() -> dict:
    manifest = accounts.sources()
    manifest["tools/validate_godot.py"] = sha(ROOT / "tools/validate_godot.py")
    return manifest


def digest(value: dict) -> str:
    return hashlib.sha256(json.dumps(value, sort_keys=True).encode()).hexdigest()


def dump(path: Path, value) -> None:
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n")


def http(base: str, path: str, token: str | None = None, method: str = "GET") -> tuple:
    request = urllib.request.Request(base + path, method=method)
    if token is not None:
        request.add_header("Authorization", "Bearer " + token)
    try:
        response = urllib.request.urlopen(request, timeout=3)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        body = response.read()
        return response.status, dict(response.headers), json.loads(body) if body and body.startswith(b"{") else body.decode()


class Monitor:
    def __init__(self, base: str, token: str, process: subprocess.Popen):
        self.base, self.token, self.process = base, token, process
        self.rows, self.errors = [], []
        self.done = threading.Event()
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def run(self):
        received = False
        while not self.done.is_set() and self.process.poll() is None:
            started = time.monotonic()
            try:
                code, _, value = http(self.base, "/admin/api/status", self.token)
                if code == 200:
                    received = True
                    self.rows.append({"elapsed_monotonic": started, "latency_ms": (time.monotonic() - started) * 1000, "response": value})
                else:
                    self.errors.append({"status": code})
            except OSError as error:
                if received and not self.done.is_set() and self.process.poll() is None:
                    self.errors.append({"kind": type(error).__name__})
            self.done.wait(.10)

    def finish(self, path: Path) -> dict:
        self.done.set()
        self.thread.join(timeout=4)
        dump(path, {"scope": "Real HTTP polling of immutable dashboard cache during actual client activity", "samples": self.rows, "errors": self.errors})
        data = [row["response"].get("data") for row in self.rows]
        data = [value for value in data if value]
        latencies = sorted(row["latency_ms"] for row in self.rows)
        return {"polls": len(self.rows), "errors": self.errors, "latency_ms_median": statistics.median(latencies) if latencies else None, "latency_ms_max": max(latencies, default=None), "max_connected": max((value["rooms"]["connected_members"] for value in data), default=0), "max_reserved": max((value["rooms"]["reserved_members"] for value in data), default=0), "cpu_percent_median": statistics.median([value["process"]["cpu_percent"] for value in data if value["process"]["cpu_percent"] is not None]) if any(value["process"]["cpu_percent"] is not None for value in data) else None, "rss_bytes_max": max((value["process"]["rss_bytes"] or 0 for value in data), default=0), "max_tick_delay_micros": max((value["metrics"]["max_tick_delay_micros"] for value in data), default=0)}


class Runner:
    def __init__(self, output: Path, godot: str):
        self.output, self.godot = output, godot
        self.processes, self.handles, self.tokens, self.monitors = [], [], [], []
        self.browse_dir = Path(tempfile.mkdtemp(prefix="campfire-dashboard-browse-"))
        self.browse_env = dict(os.environ, BROWSE_STATE_FILE=str(self.browse_dir / "state.json"))
        self.browser_started = False

    def launch(self, label, command, env=None):
        handle = (self.output / (label + ".log")).open("w")
        self.handles.append(handle)
        environment = dict(os.environ) if env is None else dict(env)
        environment.pop("CAMPFIRE_ADMIN_TOKEN", None)
        if Path(str(command[0])).name in {"campfire-server", "account_server"}:
            if env and env.get("CAMPFIRE_ADMIN_TOKEN"):
                environment["CAMPFIRE_ADMIN_TOKEN"] = env["CAMPFIRE_ADMIN_TOKEN"]
        process = subprocess.Popen(command, cwd=ROOT, env=environment, stdout=handle, stderr=subprocess.STDOUT)
        self.processes.append(process)
        return process

    def server(self, label="dashboard-server", enabled=True):
        port = accounts.free_port()
        base = f"http://127.0.0.1:{port}"
        token = secrets.token_urlsafe(36) if enabled else ""
        if token:
            self.tokens.append(token)
        env = dict(os.environ, CAMPFIRE_ADMIN_TOKEN=token)
        command = [str(BINARY), "--bind", f"127.0.0.1:{port}", "--game", "arena", "--game", "arena-b", "--capacity", "16", "--reconnect-grace-ms", "2000", "--guest-burst", "200", "--guest-rate-per-second", "200"]
        process = self.launch(label, command, env)
        accounts.wait_health(base, process)
        return process, base, token

    def browse(self, *args, chain=None):
        command = [str(BROWSE), "chain"] if chain is not None else [str(BROWSE), *map(str, args)]
        result = subprocess.run(command, cwd=ROOT, env=self.browse_env, input=json.dumps(chain) if chain is not None else None, text=True, capture_output=True, timeout=45)
        self.browser_started = True
        content = result.stdout + result.stderr
        for token in self.tokens:
            content = content.replace(token, "[REDACTED]")
        if result.returncode or "] ERROR:" in content:
            raise RuntimeError("gstack browse failed: " + content[:1800])
        return content

    def js(self, expression):
        return self.browse("js", expression)

    def until(self, expression, seconds=12):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            value = self.js("Boolean(" + expression + ")").strip()
            if value == "true":
                return
            time.sleep(.1)
        raise RuntimeError("Browser condition timed out: " + expression)

    def login(self, token):
        # gstack eval validates the safe directory then reads with readFileSync;
        # it accepts a FIFO. No key is stored in a regular file or CLI argument.
        fifo = self.browse_dir / ("auth-" + secrets.token_hex(6) + ".js")
        os.mkfifo(fifo, 0o600)
        payload = "(() => { const input=document.getElementById('admin-key'); input.value=" + json.dumps(token) + "; input.dispatchEvent(new Event('input',{bubbles:true})); document.getElementById('login-submit').click(); return input.value === ''; })()"
        def supply():
            with fifo.open("w") as stream:
                stream.write(payload)
        writer = threading.Thread(target=supply, daemon=True)
        writer.start()
        try:
            return self.browse("eval", fifo)
        finally:
            writer.join(timeout=2)
            fifo.unlink(missing_ok=True)

    def close(self):
        for monitor in self.monitors:
            monitor.done.set()
        if self.browser_started:
            try:
                self.browse("stop")
            except RuntimeError:
                pass
        for process in self.processes:
            accounts.stop(process)
        for handle in self.handles:
            handle.close()

    def scan(self):
        scanned = 0
        for root in (self.output, self.browse_dir):
            for path in root.rglob("*"):
                if path.is_file():
                    data = path.read_bytes()
                    if any(token.encode() in data for token in self.tokens):
                        raise RuntimeError("Administrator key found in " + str(path))
                    scanned += 1
        return scanned


def browser_suite(runner: Runner) -> dict:
    process, base, token = runner.server()
    checks = []
    for path in ("/admin", "/admin/assets/app.js", "/admin/assets/style.css", "/admin/api/status"):
        code, headers, body = http(base, path, token)
        lower = {key.lower(): value for key, value in headers.items()}
        assert code == 200 and "no-store" in lower["cache-control"]
        assert "content-security-policy" in lower and lower["x-content-type-options"] == "nosniff"
    checks.append("Real HTTP shell/assets/API: no-store, CSP, nosniff")
    assert http(base, "/admin/api/status")[0] == 401
    assert http(base, "/admin/api/status", "wrong")[0] == 401
    assert http(base, "/admin/api/status", token, "HEAD")[0] == 200
    assert http(base, "/admin/api/status", token, "POST")[0] == 405
    checks.append("Real HTTP missing/wrong Bearer, HEAD auth and read-only method")
    runner.browse("viewport", "1440x1100")
    runner.browse("goto", base + "/admin")
    runner.browse("screenshot", runner.output / "login-desktop.png")
    runner.login("intentionally-invalid-dashboard-key")
    runner.until("document.getElementById('login-error').textContent.includes('유효하지')")
    checks.append("Real browser wrong-key 401 login rejection")
    runner.login(token)
    runner.until("!document.getElementById('dashboard').hidden && document.getElementById('refresh').disabled === false")
    runner.until("document.getElementById('room-empty').textContent.includes('활성 룸이 없습니다')")
    runner.browse("screenshot", runner.output / "empty-desktop.png")
    _, _, initial_status = http(base, "/admin/api/status", token)
    dump(runner.output / "http-empty-status.json", initial_status)
    checks.append("Real browser authenticated empty runtime")
    with tempfile.TemporaryDirectory(prefix="campfire-dashboard-rooms-") as temporary:
        work = Path(temporary)
        fixture = runner.launch("dashboard-room-fixture", guest.engine_command(runner.godot, "res://tests/dashboard_rooms.gd", [base, work, 104]))
        guest.wait_for(lambda: guest.read_json(work / "ready.json"), 100, "104 actual dashboard rooms", [fixture])
        rooms = guest.read_json(work / "ready.json")
        dump(runner.output / "real-room-fixture.json", rooms)
        runner.browse("click", "#refresh")
        runner.until("document.querySelectorAll('#room-rows tr').length === 100")
        runner.browse("screenshot", runner.output / "dashboard-desktop.png")
        runner.browse("screenshot", "--viewport", runner.output / "dashboard-desktop-viewport.png")
        runner.browse("click", "#next-page")
        runner.until("document.querySelectorAll('#room-rows tr').length === 4 && !document.getElementById('refresh').disabled")
        runner.browse("screenshot", runner.output / "dashboard-page-two.png")
        runner.browse("click", "#previous-page")
        runner.until("document.querySelectorAll('#room-rows tr').length === 100 && !document.getElementById('refresh').disabled")
        checks.append("Actual 104 live rooms: first 100, next four, previous page")
        runner.browse("select", "#game-filter", "arena-b")
        runner.browse("select", "#visibility-filter", "private")
        runner.browse("click", "#filters button[type=submit]")
        runner.until("document.querySelectorAll('#room-rows tr').length === 1 && !document.getElementById('refresh').disabled")
        runner.browse("click", ".room-button")
        runner.until("document.getElementById('room-drawer').open")
        runner.browse("screenshot", runner.output / "dashboard-private-drawer.png")
        runner.browse("click", "#close-drawer")
        search = next(room["room_id"] for room in rooms["rooms"] if room["game_id"] == "arena-b" and not room["public"])
        runner.browse("fill", "#room-query", search)
        runner.browse("click", "#filters button[type=submit]")
        runner.until("document.querySelectorAll('#room-rows tr').length === 1 && !document.getElementById('refresh').disabled")
        checks.append("Actual multigame/private filter, exact room search and metadata drawer")
        runner.browse("click", "#reset-filters")
        runner.until("document.querySelectorAll('#room-rows tr').length === 100 && !document.getElementById('refresh').disabled")
        runner.browse("viewport", "390x844")
        runner.browse("screenshot", runner.output / "dashboard-mobile.png")
        runner.browse("screenshot", "--viewport", runner.output / "dashboard-mobile-viewport.png")
        assert runner.js("document.documentElement.scrollWidth <= innerWidth").strip() == "true"
        geometry = runner.js("JSON.stringify({viewport:innerWidth,rootClientWidth:document.documentElement.clientWidth,rootScrollWidth:document.documentElement.scrollWidth,bodyClientWidth:document.body.clientWidth,bodyScrollWidth:document.body.scrollWidth,tableClientWidth:document.querySelector('.table-scroll').clientWidth,tableScrollWidth:document.querySelector('.table-scroll').scrollWidth,tableOverflowX:getComputedStyle(document.querySelector('.table-scroll')).overflowX})")
        (runner.output / "browser-mobile-geometry.json").write_text(geometry + "\n")
        assert runner.js("document.body.scrollWidth <= innerWidth && document.querySelector('.table-scroll').scrollWidth > document.querySelector('.table-scroll').clientWidth && getComputedStyle(document.querySelector('.table-scroll')).overflowX === 'auto'").strip() == "true"
        runner.browse("viewport", "1440x1100")
        runner.until("document.querySelectorAll('#chart-connections path').length > 0")
        checks.append("Actual browser wide/narrow layout and real sampled history chart")
        for endpoint in ("status", "rooms", "history"):
            _, _, value = http(base, "/admin/api/" + endpoint, token)
            dump(runner.output / ("http-" + endpoint + ".json"), value)
            encoded = json.dumps(value)
            assert not any('"' + key + '"' in encoded for key in ("player_id", "guest_id", "claims", "credential", "settings", "private_state", "command_results", "payload"))
        checks.append("Live API allowlist excludes identity, credentials and game state fields")
        # Controlled browser fetch/visibility simulation; responses still originate
        # from the real server except the explicitly marked warming-up fixture.
        runner.browse("eval", ROOT / "tools/dashboard_browser_control.js")
        runner.js("window.__dashboardQA.mode='delay'; document.getElementById('refresh').click(); true")
        runner.until("window.__dashboardQA.pending.length === 1")
        runner.browse("click", "#auto-refresh")
        runner.js("window.__dashboardQA.release(); true")
        runner.until("!document.getElementById('refresh').disabled")
        before = runner.js("window.__dashboardQA.calls.length").strip()
        time.sleep(5.4)
        assert runner.js("window.__dashboardQA.calls.length").strip() == before
        checks.append("Controlled browser delayed real response: auto OFF permits completion, stops future poll")
        runner.js("window.__dashboardQA.mode='delay'; document.getElementById('refresh').click(); true")
        runner.until("window.__dashboardQA.pending.length === 1")
        runner.js("window.__dashboardQA.hide(); true")
        runner.until("window.__dashboardQA.aborts > 0")
        runner.js("window.__dashboardQA.release(); true")
        runner.until("!document.getElementById('refresh').disabled")
        runner.js("window.__dashboardQA.show(); true")
        runner.browse("click", "#auto-refresh")
        runner.until("!document.getElementById('refresh').disabled")
        checks.append("Controlled visibilitychange: hidden abort and visible refresh")
        runner.js("window.__dashboardQA.mode='delay'; document.getElementById('refresh').click(); true")
        runner.until("window.__dashboardQA.pending.length === 1")
        runner.browse("click", "#logout")
        runner.js("window.__dashboardQA.release(); true")
        runner.until("document.getElementById('dashboard').hidden && document.querySelectorAll('#room-rows tr').length === 0 && !document.getElementById('login-submit').disabled")
        checks.append("Controlled late real response after logout cannot restore data")
        runner.login(token)
        runner.until("!document.getElementById('dashboard').hidden && !document.getElementById('refresh').disabled")
        runner.js("window.__dashboardQA.mode='unauthorized'; document.getElementById('refresh').click(); true")
        runner.until("document.getElementById('dashboard').hidden && document.getElementById('login-error').textContent.includes('유효하지')")
        checks.append("Controlled 401 response clears authenticated UI")
        runner.js("window.__dashboardQA.mode='warming'; true")
        runner.login(token)
        runner.until("!document.getElementById('dashboard').hidden && document.getElementById('notice').textContent.includes('첫 관측')")
        runner.browse("screenshot", runner.output / "dashboard-warming-controlled.png")
        checks.append("Controlled warming-up null-data fixture displayed in actual browser")
        runner.browse("click", "#logout")
        runner.js("window.__dashboardQA.mode='cpu-zero'; true")
        runner.login(token)
        runner.until("document.getElementById('metric-cpu').textContent === '0' && !document.getElementById('refresh').disabled")
        runner.browse("screenshot", "--viewport", runner.output / "dashboard-cpu-zero-controlled.png")
        runner.js("window.__dashboardQA.mode='cpu-null'; document.getElementById('refresh').click(); true")
        runner.until("document.getElementById('metric-cpu').textContent === '—' && !document.getElementById('refresh').disabled")
        checks.append("Controlled CPU values: measured zero displays 0, unknown null displays dash")
        runner.browse("click", "#logout")
        state = runner.js("JSON.stringify({localStorageCount:localStorage.length,sessionStorageCount:sessionStorage.length,cookie:document.cookie,inputCleared:document.getElementById('admin-key').value==='',url:location.href,rows:document.querySelectorAll('#room-rows tr').length})")
        (runner.output / "browser-cleared-state.txt").write_text(state)
        assert 'localStorageCount\\":0' in state or '"localStorageCount":0' in state
        dump(runner.output / "browser-checks.json", {"checks": checks, "interaction": "gstack browse DOM/Control actions; visibility and races are JavaScript-controlled. No physical OS input claimed.", "base": base})
        (runner.output / "browser-console.txt").write_text(runner.browse("console", "--errors"))
        (work / "stop.json").write_text("{}")
        fixture.wait(timeout=8)
        assert fixture.returncode == 0
    accounts.stop(process)
    return {"checks": checks, "screenshots": [path.name for path in runner.output.glob("*.png")]}


def load_suite(runner: Runner, duration: float) -> list:
    results = []
    for enabled in (False, True):
        for count in (8, 16):
            label = f"load-{'enabled' if enabled else 'disabled'}-{count}"
            out = runner.output / label
            out.mkdir()
            server, base, token = runner.server(label + "-server", enabled)
            monitor = Monitor(base, token, server) if enabled else None
            if monitor:
                runner.monitors.append(monitor)
            samples, sampling_done = [], threading.Event()
            def sample_process():
                while not sampling_done.is_set() and server.poll() is None:
                    measured = subprocess.run(["ps", "-p", str(server.pid), "-o", "%cpu=,rss="], capture_output=True, text=True)
                    fields = measured.stdout.split()
                    if len(fields) == 2:
                        samples.append({"monotonic": time.monotonic(), "cpu_percent_ps": float(fields[0]), "rss_bytes": int(fields[1]) * 1024})
                    sampling_done.wait(.25)
            thread = threading.Thread(target=sample_process, daemon=True)
            thread.start()
            try:
                with tempfile.TemporaryDirectory(prefix="campfire-dashboard-load-") as temporary:
                    original_engine_command = guest.engine_command
                    def dashboard_engine_command(godot, script, extra, graphical=False):
                        if script == "res://tests/bot.gd":
                            script = "res://tests/dashboard_bot.gd"
                        return original_engine_command(godot, script, extra, graphical)
                    guest.engine_command = dashboard_engine_command
                    try:
                        result = guest.validate_soak(runner.godot, base, count, duration, out, Path(temporary))
                    finally:
                        guest.engine_command = original_engine_command
                measured = monitor.finish(out / "dashboard-polls.json") if monitor else None
                if enabled:
                    assert measured["polls"] >= duration * 5 and not measured["errors"]
                    assert measured["max_connected"] == count and measured["max_reserved"] >= 1
                    reserved_index = next(index for index, row in enumerate(monitor.rows) if row["response"].get("data", {}) and row["response"]["data"]["rooms"]["reserved_members"] > 0)
                    measured["connected_recovered_after_reservation"] = any(row["response"].get("data", {}) and row["response"]["data"]["rooms"]["connected_members"] == count and row["response"]["data"]["rooms"]["reserved_members"] == 0 for row in monitor.rows[reserved_index + 1:])
                    assert measured["connected_recovered_after_reservation"]
                results.append({"enabled": enabled, "count": count, "success": result["success"], "matching_public_states": len(result["common_state_checks"]), "snapshot_gap_ms_p95_max_client": max(value["snapshot_gap_ms_p95"] for value in result["clients"]), "dashboard": measured})
            finally:
                sampling_done.set()
                thread.join(timeout=3)
                dump(out / "process-os-samples.json", {"measurement": "ps reports a platform-specific average CPU (macOS: decaying average over up to one minute), an auxiliary metric distinct from sysinfo interval CPU. RSS KiB converted to bytes. Same sampling for dashboard enabled/disabled.", "samples": samples})
                accounts.stop(server)
            print(f"PASS dashboard {'enabled' if enabled else 'disabled'}: {count} real Godot clients", flush=True)
    return results


def account_suite(runner: Runner) -> dict:
    # Reuse the complete existing 86-check transport + two-native-scene runner.
    original = subprocess.Popen
    monitors = []
    token = secrets.token_urlsafe(36)
    runner.tokens.append(token)
    def hooked(command, *args, **kwargs):
        name = Path(str(command[0])).name
        if name in {"campfire-server", "account_server"}:
            environment = dict(kwargs.get("env") or os.environ)
            environment["CAMPFIRE_ADMIN_TOKEN"] = token
            kwargs["env"] = environment
        process = original(command, *args, **kwargs)
        if name in {"campfire-server", "account_server"}:
            if name == "account_server":
                address = kwargs["env"]["CAMPFIRE_DEMO_BIND"]
            else:
                address = command[command.index("--bind") + 1]
            monitor = Monitor("http://" + address, token, process)
            monitors.append((name, monitor))
        return process
    saved_argv = sys.argv
    out = runner.output / "account-regression"
    try:
        subprocess.Popen = hooked
        sys.argv = ["tools/validate_godot_accounts.py", "--skip-build", "--godot", runner.godot, "--output", str(out)]
        code = accounts.main()
    finally:
        subprocess.Popen = original
        sys.argv = saved_argv
        observations = []
        for index, (name, monitor) in enumerate(monitors):
            observations.append({"server": name, **monitor.finish(runner.output / f"account-dashboard-polls-{index}.json")})
    if code:
        raise RuntimeError("Existing account regression failed")
    if not any(item["max_connected"] >= 2 for item in observations if item["server"] == "account_server"):
        raise RuntimeError("Account dashboard did not observe native shared membership")
    player_ids = []
    for role in ("alice", "bob"):
        report = json.loads((out / (role + ".json")).read_text())
        player_ids += [item["player_id"] for item in report["observations"] if item.get("player_id")]
    checked = 0
    for _, monitor in monitors:
        for row in monitor.rows:
            encoded = json.dumps(row["response"])
            assert not any(value in encoded for value in [*player_ids, "alice-starter-item", "bob-starter-item"])
            assert not any('"' + key + '"' in encoded for key in ("player_id", "guest_id", "claims", "credential", "private_state", "command_results", "payload", "settings"))
            checked += 1
    return {"success": True, "dashboard": observations, "admin_privacy_response_checks": checked, "privacy_note": "Actual native account player IDs and starter inventory values are absent from admin HTTP responses; exact identities are not duplicated here.", "existing_result": "account-regression/summary.json"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--suite", choices=("browser", "load", "accounts", "all"), default="all")
    parser.add_argument("--godot", default=shutil.which("godot") or "/opt/homebrew/bin/godot")
    parser.add_argument("--duration", type=float, default=20)
    args = parser.parse_args()
    if not 20 <= args.duration <= 120:
        parser.error("duration must be 20..120")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    runner = Runner(output, args.godot)
    manifest = source_manifest()
    binaries = {"server": sha(BINARY), "account_server": sha(ACCOUNT), "godot": sha(Path(args.godot).resolve())}
    result = {"success": False, "timestamp_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "git_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(), "git_status_before": subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT, text=True).splitlines(), "source_manifest": manifest, "source_sha256_before": digest(manifest), "binary_sha256_before": binaries, "scope": "Real loopback HTTP only; external HTTPS/TLS deployment and WAN capacity are not tested.", "suite": args.suite, "invocation": sys.argv}
    try:
        if args.suite in ("browser", "all"):
            result["browser"] = browser_suite(runner)
        if args.suite in ("load", "all"):
            result["load"] = load_suite(runner, args.duration)
        if args.suite in ("accounts", "all"):
            result["accounts"] = account_suite(runner)
        result["success"] = True
    except Exception as error:
        result["failure"] = str(error)
        print("FAIL: " + str(error), file=sys.stderr, flush=True)
    finally:
        runner.close()
        result["source_sha256_after"] = digest(source_manifest())
        result["binary_sha256_after"] = {"server": sha(BINARY), "account_server": sha(ACCOUNT), "godot": sha(Path(args.godot).resolve())}
        if result["source_sha256_before"] != result["source_sha256_after"] or binaries != result["binary_sha256_after"]:
            result["success"] = False
            result["failure"] = "Source or binary changed during validation"
        result["owned_processes"] = [{"pid": process.pid, "exit_code": process.returncode, "alive_after_cleanup": process.poll() is None} for process in runner.processes]
        result["owned_process_cleanup_complete"] = all(process.poll() is not None for process in runner.processes)
        result["gstack_state_directory"] = str(runner.browse_dir)
        try:
            result["administrator_secret_scan_files"] = runner.scan()
        except RuntimeError as error:
            result["success"], result["failure"] = False, str(error)
        dump(output / "summary.json", result)
        print(json.dumps({"success": result["success"], "failure": result.get("failure"), "artifact": str(output)}, ensure_ascii=False), flush=True)
    return 0 if result["success"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
