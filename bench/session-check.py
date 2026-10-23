#!/usr/bin/env python3
"""Own nested compositor + private D-Bus: terminal, tray, Launchpad and Settings.

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
p.add_argument('mode',choices=['search','copy','tray','launchpad','settings','settings-restore','wallpaper','defaults','portal'])
p.add_argument('--output',type=Path)
p.add_argument('--scale',default='2')
p.add_argument('--fixture',default='/tmp/feather-tray-fixture')
p.add_argument('--screenshot-ms',type=int,default=0)
p.add_argument('--dark',action='store_true')
p.add_argument('--no-screenshot',action='store_true')
p.add_argument('--compositor',default='target/release/feather-de')
p.add_argument('--page',choices=['appearance','keyboard','displays','desktop','input','defaults','date'],default='appearance')
p.add_argument('--restore-from',type=Path)
a=p.parse_args()
root=(a.output or Path(tempfile.mkdtemp(prefix='feather-session-'))).resolve()
root.mkdir(parents=True,exist_ok=True)
env=os.environ.copy()
for k in list(env):
    if k.startswith('FEATHER_DEBUG_') or k=='FEATHER_TERMINAL_DEBUG':env.pop(k)
env.update(XDG_CONFIG_HOME=str(root/'config'),XDG_DATA_HOME=str(root/'data'),FEATHER_SCALE=a.scale,
           FEATHER_PROFILE='1',FEATHER_DEBUG_INPUT_ONLY='1',RUST_LOG='feather_de=debug,smithay=warn')
if a.restore_from:
    assert a.mode=='settings-restore'
    env['XDG_CONFIG_HOME']=str(a.restore_from.resolve()/'config');env.pop('FEATHER_SCALE',None)
if a.dark and not a.restore_from:
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
elif a.mode=='settings':
    env.update(FEATHER_DEBUG_SETTINGS='1',FEATHER_DEBUG_SETTINGS_PAGE=a.page)
    terminal=Path('target/release/feather-terminal').resolve()
    command+=['--autostart',f'echo $$ > {shlex.quote(str(root/"terminal.pid"))}; exec {shlex.quote(str(terminal))} -e sleep 35 2>{shlex.quote(str(root/"client.log"))}']
elif a.mode=='settings-restore':env['FEATHER_DEBUG_SETTINGS_RESTORE']='1'
elif a.mode=='wallpaper':
    picture=root/'мои обои.png'
    picture.write_bytes(Path('assets/wallpapers/feather-dawn.png').read_bytes())
    env.update(FEATHER_DEBUG_WALLPAPER_FILE=str(picture),FEATHER_PICKER_TEST_FILE=str(picture))
elif a.mode=='portal':
    picture=root/'выбор #1.png';picture.write_bytes(Path('assets/wallpapers/feather-dawn.png').read_bytes())
    env.update(FEATHER_PICKER_TEST_FILE=str(picture),FEATHER_PORTAL_TEST_ROOT=str(root),XDG_CURRENT_DESKTOP='FeatherDE')
    command+=['--autostart','exec /usr/bin/python3 '+shlex.quote(str(Path('bench/portal-client.py').resolve()))+f' >{shlex.quote(str(root/"client.log"))} 2>&1']
elif a.mode=='defaults':
    apps=root/'data/applications';apps.mkdir(parents=True,exist_ok=True)
    (root/'working-dir').mkdir(exist_ok=True)
    script=root/'terminal-launch.sh';terminal=Path('target/release/feather-terminal').resolve()
    script.write_text('#!/bin/sh\necho $$ >> '+shlex.quote(str(root/'terminal.pids'))+'\npwd > '+shlex.quote(str(root/'terminal-cwd'))+'\nexec '+shlex.quote(str(terminal))+' -e sleep 35\n')
    for kind,category,cmd in [('browser','WebBrowser','/usr/bin/true'),('terminal','TerminalEmulator','sh '+shlex.quote(str(script)))]:
        mime='MimeType=x-scheme-handler/http;x-scheme-handler/https;text/html;\n' if kind=='browser' else ''
        (apps/f'feather-check-{kind}.desktop').write_text(f'[Desktop Entry]\nType=Application\nName=000 Test {kind}\nCategories={category};\nExec={cmd}\n'+mime)
    env.update(FEATHER_DEBUG_DEFAULTS_ROOT=str(root),FEATHER_DEBUG_DEFAULTS_PAGE=a.page)
elif a.mode=='tray':
    env.update(FEATHER_DEBUG_TRAY='1',FEATHER_DEBUG_TRAY_CHECK='1')
    command+=['--autostart',f'QT_NO_XDG_DESKTOP_PORTAL=1 QT_QPA_PLATFORMTHEME=kde QT_QPA_PLATFORM=wayland exec {shlex.quote(a.fixture)} 2>{shlex.quote(str(root/"client.log"))}']
else:env['FEATHER_DEBUG_LAUNCHPAD_CLICK']='1'
shot=a.screenshot_ms or (12000 if a.mode in ('wallpaper','defaults') else 10000 if a.mode=='settings' else 5400 if a.mode=='tray' else 4500)
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
        samples=[];peak_anon_kib=0;peak_decoder_anon_kib=0;peak_combined_anon_kib=0;raster_task=None
        for second in range(1,21):
            deadline=time.monotonic()+1
            while time.monotonic()<deadline:
                time.sleep(min(.05,max(0,deadline-time.monotonic())))
                assert proc.poll() is None,f'compositor exited {proc.returncode}; {root}/session.log'
                resident=sample(pid)['anon_kib'];peak_anon_kib=max(peak_anon_kib,resident)
                if a.mode=='wallpaper':
                    if raster_task is None:
                        raster_task=next((t for t in Path(f'/proc/{pid}/task').iterdir() if (t/'comm').read_text().strip()=='feather-raster'),None)
                    decoder_anon=0
                    if raster_task:
                        for child in (raster_task/'children').read_text().split():
                            try:
                                if b'--decode-wallpaper' in Path(f'/proc/{child}/cmdline').read_bytes():decoder_anon+=sample(int(child))['anon_kib']
                            except (FileNotFoundError,ProcessLookupError,AssertionError,StopIteration):pass
                    peak_decoder_anon_kib=max(peak_decoder_anon_kib,decoder_anon)
                    peak_combined_anon_kib=max(peak_combined_anon_kib,resident+decoder_anon)
            assert proc.poll() is None,f'compositor exited {proc.returncode}; {root}/session.log'
            s={'second':second,**sample(pid)}
            if a.mode in ('search','copy','settings'):
                tpid=int((root/'terminal.pid').read_text());s['terminal']=sample(tpid)
            if a.mode=='copy' and second in (1,4):
                clipboard_env=env|{'WAYLAND_DISPLAY':(root/'socket').read_text()}
                text=subprocess.check_output(['wl-paste','--no-newline'],env=clipboard_env,timeout=2).decode()
                assert text==('OSC52 → Wayland' if second==1 else 'terminal-copy'),repr(text)
                s['clipboard']=text
            samples.append(s)
        log.flush();text=(root/'session.log').read_text()
        if (root/'client.log').exists():text+=(root/'client.log').read_text()
        marker={'search':'[feather-terminal-check] search','copy':'[feather-terminal-check] Shift mouse','tray':'[feather-tray-check] Qt icons','launchpad':'[feather-launchpad-check] click','settings':'[feather-settings-check] XKB','settings-restore':'[feather-settings-check] restored','wallpaper':'[feather-wallpaper-check] gallery','defaults':'[feather-defaults-check] browser','portal':'[feather-portal-check] frontend'}[a.mode]
        assert marker in text,f'check missing; {root}/session.log'
        if a.mode=='tray':assert '[feather-tray-fixture] clicked' in text
        if a.mode=='settings':
            preferences=dict(line.split('=',1) for line in (root/'config/feather-de/settings.conf').read_text().splitlines() if '=' in line)
            assert preferences['keyboard_layouts']=='us,de' and preferences['keyboard_shortcut']=='grp:ctrl_space_toggle'
            assert preferences['animations']=='true' and preferences['dock_zoom']=='true' and float(preferences['scale'])==2
        if a.mode=='wallpaper':
            preferences=dict(line.split('=',1) for line in (root/'config/feather-de/settings.conf').read_text().splitlines() if '=' in line)
            assert preferences['wallpaper_image']=='file:'+os.fsencode(picture).hex()
        if a.mode=='defaults':
            mime=(root/'config/mimeapps.list').read_text();assert 'x-scheme-handler/http=feather-check-browser.desktop;' in mime
            assert (root/'config/featherde-xdg-terminals.list').read_text().startswith('feather-check-terminal.desktop\n')
            assert (root/'terminal-cwd').read_text().strip()==str(root/'working-dir')
            terminal_pids=[int(t) for t in (root/'terminal.pids').read_text().split()];assert len(set(terminal_pids))==2
            for terminal_pid in terminal_pids:sample(terminal_pid)
        report={'mode':a.mode,'scale':a.scale,'compositor_pid':pid,'idle_ticks_5s':samples[-1]['ticks']-samples[-6]['ticks'],'peak_sampled_anon_kib':peak_anon_kib,'sample_interval_ms':50,'screenshot':(root/'frame.png').exists(),'samples':samples}
        if a.mode=='wallpaper':report.update(peak_decoder_anon_kib=peak_decoder_anon_kib,peak_combined_anon_kib=peak_combined_anon_kib)
        if a.mode in ('copy','search','settings'):report['terminal_idle_ticks_5s']=samples[-1]['terminal']['ticks']-samples[-6]['terminal']['ticks']
        (root/'report.json').write_text(json.dumps(report,ensure_ascii=False,indent=2))
        print({k:v for k,v in report.items() if k!='samples'},flush=True);print(root,flush=True)
    finally:
        try:os.killpg(proc.pid,signal.SIGTERM)
        except ProcessLookupError:pass
        try:proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid,signal.SIGKILL);proc.wait()
