#!/usr/bin/env python3
"""Own nested compositor + private D-Bus: search, clipboard, Qt tray or Launchpad.

python3 bench/session-check.py search --output /tmp/feather-search
python3 bench/session-check.py copy
python3 bench/session-check.py tray --fixture /tmp/feather-tray-fixture
No host input is injected; only the owned window is resized, verified by PID.
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

p=argparse.ArgumentParser(description=__doc__)
p.add_argument('mode',choices=['search','copy','tray','launchpad'])
p.add_argument('--output',type=Path)
p.add_argument('--scale',default='2')
p.add_argument('--fixture',default='/tmp/feather-tray-fixture')
p.add_argument('--screenshot-ms',type=int,default=0)
p.add_argument('--dark',action='store_true')
p.add_argument('--no-screenshot',action='store_true')
p.add_argument('--compositor',default='target/release/feather-de')
a=p.parse_args()
root=(a.output or Path(tempfile.mkdtemp(prefix='feather-session-'))).resolve()
root.mkdir(parents=True,exist_ok=True)
env=os.environ.copy()
for k in list(env):
    if k.startswith('FEATHER_DEBUG_') or k=='FEATHER_TERMINAL_DEBUG':env.pop(k)
env.update(XDG_CONFIG_HOME=str(root/'config'),FEATHER_SCALE=a.scale,
           FEATHER_PROFILE='1',FEATHER_DEBUG_INPUT_ONLY='1',RUST_LOG='feather_de=debug,smithay=warn')
if a.dark:
    settings=root/'config/feather-de/settings.conf';settings.parent.mkdir(parents=True,exist_ok=True)
    settings.write_text('appearance=dark\n')
command=[str(Path(a.compositor).resolve()),'--windowed']
if a.mode in ('search','copy'):
    fixture=root/'fixture.py'
    fixture.write_text('''import base64, os, time
from pathlib import Path
root=Path(__file__).parent
(root/'socket').write_text(os.environ['WAYLAND_DISPLAY'])
(root/'terminal.pid').write_text(str(os.getppid()))
if os.environ['FEATHER_TERMINAL_DEBUG']=='copy':
    print('terminal-copy',end='\\r\\n')
    print('\\x1b[?1000h\\x1b[?1006h\\x1b]52;c;'+base64.b64encode('OSC52 → Wayland'.encode()).decode()+'\\a',end='',flush=True)
else:
    for i in range(220):print(f'{i:03d} '+('Привет, Feather!' if i in (2,80,160) else 'search history line'),end='\\r\\n')
    print('',end='',flush=True)
time.sleep(35)
''')
    terminal=Path('target/release/feather-terminal').resolve()
    client=f'FEATHER_TERMINAL_DEBUG={a.mode} exec {shlex.quote(str(terminal))} -e python3 {shlex.quote(str(fixture))}'
    command+=['--autostart',client+f' 2>{shlex.quote(str(root/"client.log"))}']
elif a.mode=='tray':
    env.update(FEATHER_DEBUG_TRAY='1',FEATHER_DEBUG_TRAY_CHECK='1')
    command+=['--autostart',f'QT_NO_XDG_DESKTOP_PORTAL=1 QT_QPA_PLATFORMTHEME=kde QT_QPA_PLATFORM=wayland exec {shlex.quote(a.fixture)} 2>{shlex.quote(str(root/"client.log"))}']
else:env['FEATHER_DEBUG_LAUNCHPAD_CLICK']='1'
shot=a.screenshot_ms or (5400 if a.mode=='tray' else 4500)
if not a.no_screenshot:env.update(FEATHER_DEBUG_SCREENSHOT=str(root/'frame.png'),FEATHER_DEBUG_SCREENSHOT_MS=str(shot))

def sample(pid):
    stat=Path(f'/proc/{pid}/stat').read_text().rsplit(')',1)[1].split()
    assert stat[0]!='Z',f'process {pid} is a zombie'
    lines=Path(f'/proc/{pid}/status').read_text().splitlines()
    return {'ticks':int(stat[11])+int(stat[12]),'anon_kib':int(next(s for s in lines if s.startswith('RssAnon:')).split()[1])}

script=f'echo $$ > {shlex.quote(str(root/"compositor.pid"))}; exec '+shlex.join(command)
with (root/'session.log').open('w') as log:
    proc=subprocess.Popen(['dbus-run-session','--','sh','-c',script],env=env,stdout=log,stderr=log,start_new_session=True)
    try:
        for _ in range(100):
            if (root/'compositor.pid').exists():break
            time.sleep(.02)
        pid=int((root/'compositor.pid').read_text())
        if env.get('HYPRLAND_INSTANCE_SIGNATURE'):
            for _ in range(40):
                clients=json.loads(subprocess.check_output(['hyprctl','clients','-j']))
                if any(c['pid']==pid for c in clients):
                    for action in [f'hl.dsp.window.float({{window="pid:{pid}",action="set"}})',f'hl.dsp.window.resize({{window="pid:{pid}",x=1920,y=1080,relative=false}})']:
                        subprocess.run(['hyprctl','dispatch',action],capture_output=True,check=True)
                    break
                time.sleep(.1)
        samples=[]
        for second in range(1,21):
            time.sleep(1)
            assert proc.poll() is None,f'compositor exited {proc.returncode}; {root}/session.log'
            s={'second':second,**sample(pid)}
            if a.mode in ('search','copy'):
                tpid=int((root/'terminal.pid').read_text());s['terminal']=sample(tpid)
            if a.mode=='copy' and second in (1,4):
                clipboard_env=env|{'WAYLAND_DISPLAY':(root/'socket').read_text()}
                text=subprocess.check_output(['wl-paste','--no-newline'],env=clipboard_env,timeout=2).decode()
                assert text==('OSC52 → Wayland' if second==1 else 'terminal-copy'),repr(text)
                s['clipboard']=text
            samples.append(s)
        log.flush();text=(root/'session.log').read_text()
        if (root/'client.log').exists():text+=(root/'client.log').read_text()
        marker={'search':'[feather-terminal-check] search','copy':'[feather-terminal-check] Shift mouse','tray':'[feather-tray-check] Qt icons','launchpad':'[feather-launchpad-check] click'}[a.mode]
        assert marker in text,f'check missing; {root}/session.log'
        if a.mode=='tray':assert '[feather-tray-fixture] clicked' in text
        report={'mode':a.mode,'scale':a.scale,'compositor_pid':pid,'idle_ticks_5s':samples[-1]['ticks']-samples[-6]['ticks'],'peak_sampled_anon_kib':max(s['anon_kib'] for s in samples),'screenshot':(root/'frame.png').exists(),'samples':samples}
        if a.mode in ('copy','search'):report['terminal_idle_ticks_5s']=samples[-1]['terminal']['ticks']-samples[-6]['terminal']['ticks']
        (root/'report.json').write_text(json.dumps(report,ensure_ascii=False,indent=2))
        print({k:v for k,v in report.items() if k!='samples'},flush=True);print(root,flush=True)
    finally:
        try:os.killpg(proc.pid,signal.SIGTERM)
        except ProcessLookupError:pass
        try:proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid,signal.SIGKILL);proc.wait()
