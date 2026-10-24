#!/usr/bin/env python3
"""Raw Alt/Tab and focus races in an owned nested compositor; no host input injection.

Fixtures use mapped built-in windows with app ids/Spaces, so this checks the compositor's
selection/activation model, not delivery of keyboard events to an external client.
"""
import argparse
from collections import Counter
import json
import os
from pathlib import Path
import re
import signal
import struct
import subprocess
import tempfile
import threading
import time

p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--host',required=True)
p.add_argument('--scale',default='2')
p.add_argument('--output',type=Path)
p.add_argument('--screenshot',action='store_true')
p.add_argument('--duration',type=float,default=24)
a=p.parse_args()
if a.duration<16:p.error('--duration must be at least 16 seconds')
root=(a.output or Path(tempfile.mkdtemp(prefix='feather-switcher-'))).resolve()
root.mkdir(parents=True,exist_ok=True)
config=root/'config/feather-de';config.mkdir(parents=True,exist_ok=True)
(config/'settings.conf').write_text('show_about=false\n')
fixtures=root/'bin';fixtures.mkdir(exist_ok=True)
helper=fixtures/'pactl'
helper.write_text('#!/bin/sh\ncase "$1" in\n subscribe) exec sleep 86400;;\n --format=json) printf "[]\\n";;\n *) exit 1;;\nesac\n')
helper.chmod(0o755)
env={k:v for k,v in os.environ.items() if not k.startswith('FEATHER_DEBUG_')}
env.update(WAYLAND_DISPLAY=a.host,PATH=str(fixtures)+os.pathsep+env['PATH'],
           XDG_CONFIG_HOME=str(root/'config'),XDG_DATA_HOME=str(root/'data'),
           DBUS_SYSTEM_BUS_ADDRESS='unix:path='+str(root/'no-system-bus'),PULSE_SERVER='unix:'+str(root/'never-real-pulse'),
           FEATHER_DEBUG_SWITCHER='1',FEATHER_DEBUG_INPUT_ONLY='1',FEATHER_XWAYLAND='0',
           FEATHER_SCALE=a.scale,FEATHER_PROFILE='1',WAYLAND_DEBUG='client',RUST_LOG='feather_de=debug,smithay=warn')
if a.screenshot:env.update(FEATHER_DEBUG_SCREENSHOT=str(root/'frame.png'),FEATHER_DEBUG_SCREENSHOT_MS='5800')
pidfile=root/'pid';pidfile.unlink(missing_ok=True)
command=['dbus-run-session','--','sh','-c','echo $$ > "$1"; exec "$2" --windowed','switcher-check',str(pidfile),str(Path('target/release/feather-de').resolve())]

def sample(pid):
    stat=Path(f'/proc/{pid}/stat').read_text().rsplit(')',1)[1].split()
    status=Path(f'/proc/{pid}/status').read_text().splitlines()
    return {'ticks':int(stat[11])+int(stat[12]),'anon_kib':next(int(l.split()[1]) for l in status if l.startswith('RssAnon:'))}

with (root/'compositor.log').open('w') as log:
    process=subprocess.Popen(command,env=env,stdout=log,stderr=subprocess.PIPE,start_new_session=True)
    protocol=Counter();protocol_lock=threading.Lock();protocol_samples=[]
    def read_stderr():
        panic_lines=0
        for data in iter(process.stderr.readline,b''):
            line=data.decode('utf-8',errors='replace')
            match=re.search(r'\b([a-z][a-z0-9_]+)(?:@|#)\d+\.([A-Za-z0-9_]+)\(',line)
            if match:
                with protocol_lock:protocol['.'.join(match.groups())]+=1
            elif '[feather-' in line or 'panicked' in line or 'assertion' in line or panic_lines:
                log.write(line);log.flush()
                if 'panicked' in line:panic_lines=5
                elif panic_lines:panic_lines-=1
    reader=threading.Thread(target=read_stderr,daemon=True);reader.start()
    start=time.monotonic();samples=[];last_second=-1
    try:
        while time.monotonic()-start<a.duration:
            if process.poll() is not None:raise RuntimeError((root/'compositor.log').read_text()[-7000:])
            elapsed=time.monotonic()-start
            if pidfile.exists():
                try:row=sample(int(pidfile.read_text()))
                except (FileNotFoundError,StopIteration):
                    if process.poll() is not None:raise RuntimeError((root/'compositor.log').read_text()[-7000:])
                    continue
                row['seconds']=round(elapsed,3);samples.append(row)
                if int(elapsed)!=last_second:
                    last_second=int(elapsed)
                    with protocol_lock:protocol_samples.append({'seconds':round(elapsed,3),'counts':dict(protocol)});protocol.clear()
            time.sleep(0.05)
        text=(root/'compositor.log').read_text()
        assert '[feather-switcher-check] complete: ok' in text,text[-7000:]
        end=samples[-1];idle=max((s for s in samples if s['seconds']<=end['seconds']-5),key=lambda s:s['seconds'])
        with protocol_lock:protocol_samples.append({'seconds':round(time.monotonic()-start,3),'counts':dict(protocol)});protocol.clear()
        host_input=sum(n for row in protocol_samples if row['seconds']>=idle['seconds'] for method,n in row['counts'].items() if method.startswith(('wl_pointer.','wl_keyboard.','wl_touch.')))
        report={'scale':a.scale,'pid':int(pidfile.read_text()),'peak_anon_kib':max(s['anon_kib'] for s in samples),
                'steady_anon_kib':end['anon_kib'],'idle_ticks_5s':end['ticks']-idle['ticks'],
                'idle_window_seconds':round(end['seconds']-idle['seconds'],3),'idle_host_input_events':host_input,
                'screenshot':(root/'frame.png').exists(),'samples':samples,'protocol_samples':protocol_samples}
        (root/'report.json').write_text(json.dumps(report,indent=2))
        print(json.dumps({k:v for k,v in report.items() if k not in ('samples','protocol_samples')}))
        assert host_input==0,'host input contaminated idle measurement'
        assert report['idle_ticks_5s']==0
        if not a.screenshot:assert report['peak_anon_kib']*1024<=60_000_000
        else:
            w,h=struct.unpack('>II',(root/'frame.png').read_bytes()[16:24])
            assert report['peak_anon_kib']*1024<=60_000_000+2*w*h*4
            peak=max(samples,key=lambda s:s['anon_kib'])
            assert any(peak['seconds']<=s['seconds']<=peak['seconds']+2 and s['anon_kib']*1024<=60_000_000 for s in samples)
    finally:
        if process.poll() is None:
            os.killpg(process.pid,signal.SIGTERM)
            try:process.wait(timeout=4)
            except subprocess.TimeoutExpired:os.killpg(process.pid,signal.SIGKILL);process.wait()
        reader.join(timeout=4)
