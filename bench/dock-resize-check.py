#!/usr/bin/env python3
"""Exercise only an owned nested compositor; never sends input to the host.

python3 bench/dock-resize-check.py resize --output /tmp/feather-resize
python3 bench/dock-resize-check.py slow --screenshot-ms 2900
Hyprland geometry changes address only a freshly observed compositor PID.
"""
import argparse
import json
import os
from pathlib import Path
import shlex
import signal
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('mode', choices=['resize', 'dock', 'slow', 'pins'])
parser.add_argument('--output', type=Path)
parser.add_argument('--screenshot-ms', type=int, default=0)
parser.add_argument('--scale', default='2')
args = parser.parse_args()
root = args.output or Path(tempfile.mkdtemp(prefix='feather-dock-resize-'))
root.mkdir(parents=True, exist_ok=True)
env = os.environ.copy()
for key in list(env):
    if key.startswith('FEATHER_DEBUG_'):
        env.pop(key)
env.update(XDG_CONFIG_HOME=str(root/'config'), FEATHER_SCALE=args.scale,
           FEATHER_PROFILE='1', FEATHER_DEBUG_INPUT_ONLY='1', RUST_LOG='feather_de=debug,smithay=warn')
command = ['target/release/feather-de', '--windowed']
if args.mode == 'pins':
    config = root/'config'/'feather-de'
    config.mkdir(parents=True, exist_ok=True)
    (config/'settings.conf').write_text('show_about=false\ndock=builtin.launchpad,dolphin\n')
    env['FEATHER_DEBUG_DOCK_PINS'] = '1'
elif args.mode == 'resize':
    env['FEATHER_DEBUG_DND'] = 'resize-all'
    client = Path('target/release/feather-terminal').resolve()
    script = (f'echo $$ > {shlex.quote(str(root.resolve()/"terminal.pid"))}; '
              f'WAYLAND_DEBUG=1 RUST_BACKTRACE=1 exec {shlex.quote(str(client))} '
              f'2>{shlex.quote(str(root.resolve()/"terminal.log"))}')
    command += ['--autostart', script]
else:
    env['FEATHER_DEBUG_DOCK'] = '1'
    if args.mode == 'slow':
        env['FEATHER_DEBUG_DOCK_SLOW'] = '1'
if args.screenshot_ms:
    env.update(FEATHER_DEBUG_SCREENSHOT=str(root.resolve()/'frame.png'),
               FEATHER_DEBUG_SCREENSHOT_MS=str(args.screenshot_ms))


def sample(pid):
    stat = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
    status = Path(f'/proc/{pid}/status').read_text().splitlines()
    return {'ticks': int(stat[11])+int(stat[12]),
            'anon_kib': int(next(s for s in status if s.startswith('RssAnon:')).split()[1])}


def active_vt():
    return Path('/sys/class/tty/tty0/active').read_text().strip()


started_vt = active_vt()
samples = []
with (root/'compositor.log').open('w') as log:
    proc = subprocess.Popen(command, env=env, stdout=log, stderr=log, start_new_session=True)
    try:
        if os.environ.get('HYPRLAND_INSTANCE_SIGNATURE'):
            for _ in range(40):
                time.sleep(.1)
                clients = json.loads(subprocess.check_output(['hyprctl', 'clients', '-j']))
                if any(c['pid'] == proc.pid for c in clients):
                    # The project's current host uses Hyprland 0.56's Lua dispatch API.
                    for action in [f'hl.dsp.window.float({{window="pid:{proc.pid}", action="set"}})',
                                   f'hl.dsp.window.resize({{window="pid:{proc.pid}",x=1920,y=1080,relative=false}})']:
                        subprocess.run(['hyprctl', 'dispatch', action], capture_output=True, check=True)
                    break
        for second in range(1, 21):
            time.sleep(1)
            if proc.poll() is not None:
                raise RuntimeError(f'compositor exited {proc.returncode}; see {root}/compositor.log')
            entry = {'second': second, **sample(proc.pid)}
            if args.mode == 'resize':
                pid = int((root/'terminal.pid').read_text())
                stat = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
                assert stat[0] != 'Z', f'terminal died; see {root}/terminal.log'
                entry['terminal_pid'] = pid
            samples.append(entry)
        log.flush()
        text = (root/'compositor.log').read_text()
        if args.mode == 'resize':
            assert text.count('[feather-resize-check] pass=') == 16
            assert '[feather-resize-check] unaligned SHM size passed' in text
        elif args.mode == 'pins':
            assert text.count('[feather-dock-pins-check]') == 6
            settings = (root/'config'/'feather-de'/'settings.conf').read_text()
            assert 'dock_locked=true\n' in settings, 'Dock lock was not persisted'
            pins = next(line[5:] for line in settings.splitlines() if line.startswith('dock='))
            assert pins.split(',')[-1] == 'dolphin', 'Files position was not persisted'
        else:
            assert text.count('[feather-dock-check] same-point click') == 4
            assert '[feather-dock-check] three windows' in text
            assert '[feather-dock-check] Alt+Tab icon click' in text
        report = {'mode': args.mode, 'compositor_pid': proc.pid,
                  'idle_ticks_5s': samples[19]['ticks']-samples[14]['ticks'],
                  'peak_sampled_anon_kib': max(s['anon_kib'] for s in samples),
                  'vt_start': started_vt, 'vt_end': active_vt(),
                  'host_display': env.get('WAYLAND_DISPLAY'), 'scale': args.scale,
                  'samples': samples}
        (root/'report.json').write_text(json.dumps(report, indent=2))
        print({k: v for k, v in report.items() if k != 'samples'}, flush=True)
        print(root, flush=True)
    finally:
        # This process group was created above and contains only this test and its clients.
        try:
            os.killpg(proc.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGKILL)
            proc.wait()
