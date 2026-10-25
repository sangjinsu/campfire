"""Repeat the 20 real-browser layout and controlled-history checks."""
from __future__ import annotations

import hashlib
import json
from pathlib import Path
import tempfile
import time

import validate_godot as guest

ROOT = Path(__file__).resolve().parents[1]


def run_ui_probe(runner, http, sha, dump) -> dict:
    output = runner.output / "ui-probe"
    output.mkdir()
    checks: dict[str, dict] = {}
    failures: list[str] = []

    def browser_json(expression: str):
        return json.loads(runner.js("JSON.stringify(" + expression + ")"))

    def record(label: str, success: bool, details) -> None:
        checks[label] = {"success": bool(success), "details": details}
        if not success:
            failures.append(label)

    def layout() -> dict:
        return browser_json("""(() => {
          const table = document.getElementById('room-table-scroll');
          const box = element => { const rect = element.getBoundingClientRect(); return { x: rect.x, y: rect.y, width: rect.width, height: rect.height }; };
          const charts = [...document.querySelectorAll('svg.chart')].map(svg => {
            const labels = [...svg.querySelectorAll('text')].map(element => {
              const matrix = element.getScreenCTM();
              const font = parseFloat(getComputedStyle(element).fontSize);
              return { text: element.textContent, effectiveFontPx: font * Math.hypot(matrix.c, matrix.d), bbox: box(element) };
            });
            const overlaps = [];
            for (let i = 0; i < labels.length; i++) for (let j = i + 1; j < labels.length; j++) {
              const a = labels[i].bbox, b = labels[j].bbox;
              if (Math.min(a.x + a.width, b.x + b.width) - Math.max(a.x, b.x) > .5 && Math.min(a.y + a.height, b.y + b.height) - Math.max(a.y, b.y) > .5) overlaps.push([i, j]);
            }
            return { id: svg.id, labels, overlaps };
          });
          const targets = [...document.querySelectorAll('button,input,select')].filter(element => !element.closest('[hidden]') && element.getClientRects().length).map(element => {
            const target = element.type === 'checkbox' ? element.closest('label') : element;
            return { id: element.id || element.className, tag: element.tagName, bbox: box(target) };
          });
          return { viewport: innerWidth, rootWidth: document.documentElement.scrollWidth, bodyWidth: document.body.scrollWidth, charts, targets,
            table: { rows: document.querySelectorAll('#room-rows tr').length, clientHeight: table.clientHeight, scrollHeight: table.scrollHeight,
              sticky: [...document.querySelectorAll('th')].map(element => ({ position: getComputedStyle(element).position, top: getComputedStyle(element).top })) } };
        })()""")

    server = fixture = None
    try:
        server, base, token = runner.server("ui-probe-server")
        runner.browse("goto", base + "/admin")
        runner.login(token)
        runner.until("!document.getElementById('dashboard').hidden && !document.getElementById('refresh').disabled")
        with tempfile.TemporaryDirectory(prefix="campfire-ui-probe-rooms-") as directory:
            work = Path(directory)
            fixture = runner.launch("ui-probe-rooms", guest.engine_command(runner.godot, "res://tests/dashboard_rooms.gd", [base, work, 104]))
            guest.wait_for(lambda: guest.read_json(work / "ready.json"), 100, "104 actual probe rooms", [fixture])
            runner.browse("click", "#refresh")
            runner.until("document.querySelectorAll('#room-rows tr').length === 100 && !document.getElementById('refresh').disabled")
            for width, height in ((1440, 1100), (390, 844)):
                runner.browse("viewport", f"{width}x{height}")
                runner.js("window.scrollTo(0,0); true")
                value = layout()
                dump(output / f"layout-{width}.json", value)
                fonts = [label["effectiveFontPx"] for chart in value["charts"] for label in chart["labels"]]
                overlaps = [(chart["id"], chart["overlaps"]) for chart in value["charts"] if chart["overlaps"]]
                small = [target for target in value["targets"] if target["bbox"]["height"] < 43.99 or (target["tag"] == "BUTTON" and target["bbox"]["width"] < 43.99)]
                table = value["table"]
                record(f"font-at-least-12-{width}", bool(fonts) and min(fonts) >= 12, {"minimum_effective_px": min(fonts) if fonts else None})
                record(f"no-svg-label-overlap-{width}", not overlaps, overlaps)
                record(f"touch-targets-44-{width}", not small, small)
                record(f"bounded-sticky-table-{width}", table["rows"] == 100 and table["clientHeight"] <= 440 and table["scrollHeight"] > table["clientHeight"] and bool(table["sticky"]) and all(item["position"] == "sticky" and item["top"] == "0px" for item in table["sticky"]), table)
                record(f"no-body-horizontal-overflow-{width}", value["rootWidth"] <= width and value["bodyWidth"] <= width, {"root": value["rootWidth"], "body": value["bodyWidth"]})
                runner.browse("screenshot", "--viewport", output / f"actual-layout-{width}.png")
            runner.browse("viewport", "1440x1100")
            before = browser_json("""(() => { const table = document.getElementById('room-table-scroll'); table.scrollTop = 300;
              const button = document.querySelectorAll('.room-button')[5]; button.focus({ preventScroll: true });
              return { scroll: table.scrollTop, room: button.textContent, footer: document.getElementById('footer-status').textContent,
                headY: document.querySelector('th').getBoundingClientRect().top, scrollY: table.getBoundingClientRect().top }; })()""")
            time.sleep(5.6)
            after = browser_json("({scroll:document.getElementById('room-table-scroll').scrollTop,room:document.activeElement.textContent,footer:document.getElementById('footer-status').textContent})")
            record("automatic-five-second-scroll-focus-preserved", before["scroll"] == after["scroll"] == 300 and before["room"] == after["room"] and before["footer"] != after["footer"], {"before": before, "after": after})
            record("sticky-header-screen-position", abs(before["headY"] - before["scrollY"]) < 3, {"headY": before["headY"], "scrollY": before["scrollY"]})
            runner.browse("click", "#next-page")
            runner.until("document.querySelectorAll('#room-rows tr').length === 4 && !document.getElementById('refresh').disabled")
            page = browser_json("({top:document.getElementById('room-table-scroll').scrollTop,previousEnabled:!document.getElementById('previous-page').disabled})")
            record("page-resets-table-scroll", page["top"] == 0 and page["previousEnabled"], page)
            runner.browse("click", "#previous-page")
            runner.until("document.querySelectorAll('#room-rows tr').length === 100 && !document.getElementById('refresh').disabled")
            runner.js("document.getElementById('room-table-scroll').scrollTop = 300; true")
            runner.browse("select", "#game-filter", "arena-b")
            runner.browse("click", "#filters button[type=submit]")
            runner.until("document.querySelectorAll('#room-rows tr').length === 2 && !document.getElementById('refresh').disabled")
            filtered = browser_json("({top:document.getElementById('room-table-scroll').scrollTop,text:document.getElementById('filter-description').textContent})")
            record("filter-resets-table-scroll", filtered["top"] == 0 and "arena-b" in filtered["text"], filtered)
            runner.browse("click", "#reset-filters")
            runner.until("document.querySelectorAll('#room-rows tr').length === 100 && !document.getElementById('refresh').disabled")
            runner.browse("eval", ROOT / "tools/dashboard_history_control.js")
            expected = {"empty": "첫 이력", "one": "관측 시작", "eleven": "관측 10초", "fifteen": "관측 15분 0초", "gap-null-multicore": "125%"}
            for case, phrase in expected.items():
                runner.js("window.__historyCase=" + json.dumps(case) + ";document.getElementById('refresh').click();true")
                runner.until("!document.getElementById('refresh').disabled")
                value = browser_json("({description:document.getElementById('history-description').textContent,cpuValue:document.getElementById('chart-cpu-value').textContent})")
                dump(output / f"controlled-history-{case}.json", value)
                runner.browse("screenshot", "--viewport", output / f"controlled-history-{case}.png")
                observed = value["cpuValue"] if case == "gap-null-multicore" else value["description"]
                record("controlled-history-" + case, phrase in observed, value)
            served = {}
            for name, route in (("index.html", "/admin"), ("app.js", "/admin/assets/app.js"), ("style.css", "/admin/assets/style.css")):
                code, _, body = http(base, route)
                served[name] = {"status": code, "served": hashlib.sha256(body.encode()).hexdigest(), "source": sha(ROOT / "crates/campfire-server/src/dashboard/assets" / name)}
            record("actual-served-assets-match-source", all(value["status"] == 200 and value["served"] == value["source"] for value in served.values()), served)
            (work / "stop.json").write_text("{}")
            fixture.wait(timeout=8)
            if fixture.returncode:
                raise RuntimeError("UI probe room fixture failed")
        result = {"success": not failures, "checks": checks, "failures": failures, "scope": "Real loopback server and Godot rooms; Chromium layout is measured. History edge cases use controlled fetch responses."}
        dump(output / "summary.json", result)
        if failures:
            raise RuntimeError("UI probe failed: " + ", ".join(failures))
        return result
    except Exception as error:
        dump(output / "summary.json", {"success": False, "checks": checks, "failures": failures, "failure": str(error), "scope": "Real Chromium layout and controlled history"})
        raise
    finally:
        if fixture is not None and fixture.poll() is None:
            guest.terminate([fixture])
        if server is not None:
            import validate_godot_accounts as accounts
            accounts.stop(server)
