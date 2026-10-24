#!/usr/bin/env python3
"""Keyboard settings and field repeat inside an owned nested compositor; no host input.

Uses a private D-Bus and preferences directory. The host Wayland socket must be selected
explicitly; only this script's PIDs are stopped. --no-screenshot measures memory without
readback allocations. Each run also restarts the compositor to verify saved bindings.
"""
import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--host', required=True, help='verified host Wayland socket, e.g. wayland-1')
p.add_argument('--output', type=Path)
p.add_argument('--scale', default='1')
p.add_argument('--no-screenshot', action='store_true')
a = p.parse_args()
root = (a.output or Path(tempfile.mkdtemp(prefix='feather-keyboard-'))).resolve()
root.mkdir(parents=True, exist_ok=True)
binary = Path('target/release/feather-de').resolve()

def sample(pid):
    proc = Path(f'/proc/{pid}')
    stat = (proc / 'stat').read_text().rsplit(')', 1)[1].split()
    status = (proc / 'status').read_text().splitlines()
    anon = next(int(line.split()[1]) for line in status if line.startswith('RssAnon:'))
    return {'ticks': int(stat[11]) + int(stat[12]), 'anon_kib': anon}

def run(mode, seconds):
    folder = root / mode
    folder.mkdir(exist_ok=True)
    env = {k: v for k, v in os.environ.items() if not k.startswith('FEATHER_DEBUG_')}
    env.update(WAYLAND_DISPLAY=a.host, XDG_CONFIG_HOME=str(root / 'config'),
               XDG_DATA_HOME=str(root / 'data'), FEATHER_SCALE=a.scale, FEATHER_XWAYLAND='0',
               FEATHER_DEBUG_KEYBOARD=mode, FEATHER_DEBUG_INPUT_ONLY='1', FEATHER_PROFILE='1',
               RUST_LOG='feather_de=debug,smithay=warn')
    if mode == 'check' and not a.no_screenshot:
        env.update(FEATHER_DEBUG_SCREENSHOT=str(folder / 'frame.png'), FEATHER_DEBUG_SCREENSHOT_MS='8200')
    (folder / 'pid').unlink(missing_ok=True)
    script = 'echo $$ > "$1"; exec "$2" --windowed'
    with (folder / 'compositor.log').open('w') as log:
        process = subprocess.Popen(['dbus-run-session', '--', 'sh', '-c', script,
                                    'keyboard-check', str(folder / 'pid'), str(binary)],
                                   env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        start = time.monotonic()
        samples = []
        try:
            while time.monotonic() - start < seconds:
                if process.poll() is not None:
                    raise RuntimeError((folder / 'compositor.log').read_text()[-5000:])
                pidfile = folder / 'pid'
                if pidfile.exists():
                    value = sample(int(pidfile.read_text()))
                    value['seconds'] = round(time.monotonic() - start, 3)
                    samples.append(value)
                time.sleep(0.05)
            text = (folder / 'compositor.log').read_text()
            expected = f'[feather-keyboard-check] {"complete" if mode == "check" else "restored"}: ok'
            assert expected in text, text[-5000:]
            end = samples[-1]
            idle = max((s for s in samples if s['seconds'] <= end['seconds'] - 5), key=lambda s: s['seconds'])
            report = {'mode': mode, 'pid': int(pidfile.read_text()),
                      'peak_anon_kib': max(s['anon_kib'] for s in samples),
                      'steady_anon_kib': end['anon_kib'],
                      'idle_ticks_5s': end['ticks'] - idle['ticks'],
                      'idle_window_seconds': round(end['seconds'] - idle['seconds'], 3),
                      'sample_interval_ms': 50,
                      'screenshot': (folder / 'frame.png').exists(), 'samples': samples}
            (folder / 'report.json').write_text(json.dumps(report, indent=2))
            print(json.dumps({k: v for k, v in report.items() if k != 'samples'}))
            assert report['idle_ticks_5s'] == 0, 'idle CPU must stop after releasing all keys'
            if a.no_screenshot or mode == 'restore':
                assert report['peak_anon_kib'] * 1024 <= 60_000_000, 'private memory budget'
            else:
                import struct
                width, height = struct.unpack('>II', (folder / 'frame.png').read_bytes()[16:24])
                assert report['peak_anon_kib'] * 1024 <= 60_000_000 + 2 * width * height * 4, 'readback peak budget'
                assert max(s['anon_kib'] for s in samples if s['seconds'] < 8.1) * 1024 <= 60_000_000
                assert any(8.2 < s['seconds'] <= 10.2 and s['anon_kib'] * 1024 <= 60_000_000 for s in samples)

        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=4)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()

run('check', 17)
run('restore', 9)
