#!/usr/bin/env python3
"""Own nested compositor only; no synthetic input or user-session restart.

python3 bench/raster-check.py --before /tmp/feather-before --visual
Runs on the active host in WAYLAND_DISPLAY. Hyprland windows are matched by PID.
"""
import argparse
import json
import os
import re
from pathlib import Path
import signal
import subprocess
import tempfile
import threading
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--binary", type=Path, default=Path("target/release/feather-de"))
parser.add_argument("--before", type=Path)
parser.add_argument("--output", type=Path)
parser.add_argument("--visual", action="store_true")
parser.add_argument("--visual-ui", action="store_true", help="menu/Control Center before and after, plus scale/theme stress")
parser.add_argument("--screenshot-ms", type=int, default=6500)
parser.add_argument("--width", type=int, default=1440, help="own host window, logical pixels")
parser.add_argument("--height", type=int, default=900)
parser.add_argument("--memory-sample-ms", type=int, default=0, help="additional RSS sampling, e.g. 100")
args = parser.parse_args()
if args.width <= 0 or args.height <= 0 or args.memory_sample_ms < 0 or args.screenshot_ms <= 0:
    parser.error("dimensions must be positive; sample period must be non-negative")
root = args.output or Path(tempfile.mkdtemp(prefix="feather-raster-"))
root.mkdir(parents=True, exist_ok=True)


def sample(pid):
    stat = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    status = Path(f"/proc/{pid}/status").read_text().splitlines()
    return {
        "ticks": int(stat[11]) + int(stat[12]),
        "rss_anon_kib": int(next(s for s in status if s.startswith("RssAnon:")).split()[1]),
        "rss_file_kib": int(next(s for s in status if s.startswith("RssFile:")).split()[1]),
        "rss_total_kib": int(next(s for s in status if s.startswith("VmRSS:")).split()[1]),
    }


def active_vt():
    try:
        return Path("/sys/class/tty/tty0/active").read_text().strip()
    except OSError:
        return None


def own_window(pid):
    if not os.environ.get("HYPRLAND_INSTANCE_SIGNATURE"):
        return
    # Never move a window unless its compositor process was just observed in the host.
    deadline = time.monotonic() + 3
    version = subprocess.check_output(["hyprctl", "version"], text=True)
    match = re.search(r"Hyprland (\d+)\.(\d+)", version)
    lua = match and tuple(map(int, match.groups())) >= (0, 55)
    while time.monotonic() < deadline:
        found = subprocess.run(["hyprctl", "clients", "-j"], capture_output=True, text=True)
        if found.returncode:
            return
        client = next((c for c in json.loads(found.stdout) if c["pid"] == pid), None)
        if client:
            if lua:
                actions = [
                    [f'hl.dsp.window.float({{window="pid:{pid}", action="set"}})'],
                    [f'hl.dsp.window.resize({{window="pid:{pid}", x={args.width}, y={args.height}, relative=false}})'],
                ]
            else:
                actions = [] if client["floating"] else [["togglefloating", f"pid:{pid}"]]
                actions.append(["resizewindowpixel", f"exact {args.width} {args.height},pid:{pid}"])
            for action in actions:
                result = subprocess.run(["hyprctl", "dispatch", *action], capture_output=True, text=True)
                if result.returncode:
                    raise RuntimeError(f"{action} for own PID {pid}: {result.stdout} {result.stderr}")
            clients = json.loads(subprocess.check_output(["hyprctl", "clients", "-j"]))
            return next((c["size"] for c in clients if c["pid"] == pid), None)
        time.sleep(0.1)


def run(name, binary, mode):
    started_vt = active_vt()
    env = os.environ.copy()
    for key in list(env):
        if key.startswith("FEATHER_DEBUG_") or key == "FEATHER_APPMENU_REGISTRAR":
            env.pop(key)
    env.update(FEATHER_PROFILE="1", FEATHER_SCALE="2", XDG_CONFIG_HOME=str(root / f"config-{name}"))
    visual = mode != "profile"
    if mode == "profile":
        env["FEATHER_DEBUG_ANIMATIONS"] = "1"
    elif mode == "stress":
        env["FEATHER_DEBUG_RASTER_STRESS"] = "1"
    else:
        env["FEATHER_DEBUG_OPEN"] = mode
    if visual:
        env.update(FEATHER_DEBUG_SCREENSHOT=str(root / f"{name}.png"), FEATHER_DEBUG_SCREENSHOT_MS=str(args.screenshot_ms))
    measurements = []
    memory_samples = []
    stop_sampling = threading.Event()

    def sample_memory(pid):
        started = time.monotonic()
        while not stop_sampling.is_set():
            try:
                memory_samples.append({"second": round(time.monotonic() - started, 3), **sample(pid)})
            except (FileNotFoundError, ProcessLookupError, StopIteration):
                break
            stop_sampling.wait(args.memory_sample_ms / 1000)

    with (root / f"{name}.log").open("w") as log:
        proc = subprocess.Popen([str(binary.resolve()), "--windowed"], env=env, stdout=log, stderr=log, start_new_session=True)
        sampler = None
        try:
            if args.memory_sample_ms:
                sampler = threading.Thread(target=sample_memory, args=(proc.pid,), daemon=True)
                sampler.start()
            geometry = own_window(proc.pid)
            for second in range(max(3, args.screenshot_ms // 1000 + 2) if visual else 17):
                time.sleep(1)
                if proc.poll() is not None:
                    raise RuntimeError(f"{name} exited early: {proc.returncode}")
                measurements.append({"second": second + 1, **sample(proc.pid)})
        finally:
            stop_sampling.set()
            if sampler:
                sampler.join(timeout=2)
            if proc.poll() is None:
                os.killpg(proc.pid, signal.SIGTERM)
                try:
                    proc.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    os.killpg(proc.pid, signal.SIGKILL)
                    proc.wait()
    report = {
        "samples": measurements,
        "host_window_size": geometry,
        "host_display": env.get("WAYLAND_DISPLAY"),
        "active_vt_start": started_vt,
        "active_vt_end": active_vt(),
    }
    if memory_samples:
        report["memory_samples"] = memory_samples
        report["memory_sample_ms"] = args.memory_sample_ms
        report["peak_sampled_rss_anon_kib"] = max(s["rss_anon_kib"] for s in memory_samples)
    if not visual:
        report["idle_5s_ticks"] = measurements[15]["ticks"] - measurements[10]["ticks"]
        report["final_rss_anon_kib"] = measurements[-1]["rss_anon_kib"]
    (root / f"{name}.json").write_text(json.dumps(report, indent=2))
    print(name, {k: v for k, v in report.items() if k not in ("samples", "memory_samples")}, flush=True)


if args.before:
    run("before", args.before, "profile")
run("after", args.binary, "profile")
if args.visual:
    if args.before:
        run("before-launchpad", args.before, "launchpad")
    run("after-launchpad", args.binary, "launchpad")
    run("after-cc", args.binary, "cc")
    run("stress", args.binary, "stress")
if args.visual_ui:
    for mode in ("menu-app", "cc"):
        if args.before:
            run(f"before-{mode}", args.before, mode)
        run(f"after-{mode}", args.binary, mode)
    run("stress", args.binary, "stress")
print(root, flush=True)
