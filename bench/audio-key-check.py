#!/usr/bin/env python3
"""Owned nested compositor + fake pactl. Never changes the user's real audio server.

The fake transport tests the real worker, subscription, raw evdev keys, held key timers,
HUD and CC snapshots. --microphone exercises Sound settings, slow dragging and hotplug.
The host socket must be selected explicitly after checking its owner.
"""
import argparse
import fcntl
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time
import re
import threading
from collections import Counter

p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--host',required=True)
p.add_argument('--system-sounds',action='store_true',help='exercise sound preferences and fake shared player')
p.add_argument('--restore-sounds',action='store_true',help='reuse system-sounds settings and check persistence')
p.add_argument('--microphone',action='store_true',help='exercise Sound settings and microphone hotplug instead of media keys')
p.add_argument('--scale',default='2')
p.add_argument('--output',type=Path)
p.add_argument('--screenshot',action='store_true')
p.add_argument('--duration',type=float,default=24)
p.add_argument('--protocol-summary',action='store_true',help='count host protocol method names; discard all arguments')
a=p.parse_args()
if a.restore_sounds and not a.system_sounds:p.error('--restore-sounds requires --system-sounds')
if a.system_sounds and a.microphone:p.error('select one sound scenario')
if a.duration < 18:p.error("--duration must be at least 18 seconds")
sources=[
    {'index':2,'name':'builtin mic','description':'Встроенный микрофон','mute':False,'monitor_source':'',
     'volume':{'left':{'value':49152},'right':{'value':24576}},
     'ports':[{'name':'front','availability':'not available'}],'active_port':'front'},
    {'index':3,'name':'usb mic','description':'USB · Студийный микрофон','mute':False,'monitor_source':'',
     'volume':{'mono':{'value':19661}},'ports':[]},
    {'index':4,'name':'output.monitor','description':'Output monitor','mute':False,'monitor_source':'built-in output',
     'volume':{'mono':{'value':65536}}},
]
root=(a.output or Path(tempfile.mkdtemp(prefix='feather-audio-keys-'))).resolve()
root.mkdir(parents=True,exist_ok=True)
fixture=root/'bin';fixture.mkdir(exist_ok=True)
fifo=root/'events'
if not fifo.exists():os.mkfifo(fifo)
statefile=root/'audio.json'
statefile.write_text(json.dumps({'default_source':'builtin mic','sources':sources,'default':'built-in output','sinks':[
    {'index':7,'name':'built-in output','description':'Built-in speakers','mute':False,
     'volume':{'left':{'value':16384},'right':{'value':32768}}},
    {'index':8,'name':'usb output','description':'USB speakers','mute':False,'volume':{'mono':{'value':19661}}}
]}))
(root/'calls.jsonl').write_text('')
helper=fixture/'pactl'
helper.write_text('''#!/usr/bin/python3
import fcntl,json,os,sys
from pathlib import Path
root=Path(os.environ['FEATHER_FAKE_AUDIO'])
args=sys.argv[1:]
if args==['subscribe']:
    fd=os.open(root/'events',os.O_RDWR)
    while True:
        data=os.read(fd,4096)
        if data:
            kind='source' if b'source' in data else 'server' if b'server' in data else 'sink'
            print("Event 'change' on "+kind+" #2",flush=True)
with (root/'audio.lock').open('a') as lock:
    fcntl.flock(lock,fcntl.LOCK_EX)
    state=json.loads((root/'audio.json').read_text())
    if args==['get-default-sink']:print(state['default'])
    elif args==['--format=json','list','sinks']:print(json.dumps(state['sinks']))
    elif args==['get-default-source']:print(state['default_source'])
    elif args==['--format=json','list','sources']:
        if state.get('input_error'):raise SystemExit('Input service unavailable')
        print(json.dumps(state['sources']))
    elif args==['--format=json','list','source-outputs']:
        print('[{"index":10,"source":2},{"index":11,"source":99}]')
    elif args[0] in ['set-default-source','set-source-volume','set-source-mute','move-source-output']:
        if args[0]=='set-source-mute' and state.get('reject_mute'):
            state['reject_mute']=False
            state['sinks'][1]['mute']=True
            (root/'audio.json').write_text(json.dumps(state))
            raise SystemExit('Microphone command rejected')
        if args[0]=='set-default-source':state['default_source']=args[1]
        elif args[0]!='move-source-output':
            source=next(s for s in state['sources'] if s['name']==args[1])
            if args[0]=='set-source-volume':
                for value,percent in zip(source['volume'].values(),args[2:]):value['value']=round(float(percent.rstrip('%'))*65536/100)
            else:source['mute']=args[2]=='1'
        (root/'audio.json').write_text(json.dumps(state))
        with (root/'calls.jsonl').open('a') as log:log.write(json.dumps(args)+'\\n')
        fd=os.open(root/'events',os.O_WRONLY|os.O_NONBLOCK)
        try:os.write(fd,b'source\\n')
        finally:os.close(fd)
    elif args[0] in ['set-sink-volume','set-sink-mute']:
        sink=next(s for s in state['sinks'] if s['name']==args[1])
        if args[0]=='set-sink-volume':
            for (channel,value),percent in zip(sink['volume'].items(),args[2:]):
                value['value']=round(float(percent.rstrip('%'))*65536/100)
        else:sink['mute']=args[2]=='1'
        (root/'audio.json').write_text(json.dumps(state))
        with (root/'calls.jsonl').open('a') as log:log.write(json.dumps(args)+'\\n')
        fd=os.open(root/'events',os.O_WRONLY|os.O_NONBLOCK)
        try:os.write(fd,b'sink\\n')
        finally:os.close(fd)
    else:raise SystemExit('unexpected fake pactl args: '+repr(args))
''')
helper.chmod(0o755)
if a.system_sounds:
    player=fixture/'pw-play'
    player.write_text('''#!/usr/bin/python3
import json,os,sys,time,wave
from pathlib import Path
root=Path(os.environ['FEATHER_FAKE_AUDIO'])
with wave.open(sys.argv[1],'rb') as sound:
    frames=sound.getnframes();pcm=sound.readframes(frames)
import struct
values=struct.unpack('<'+'h'*(len(pcm)//2),pcm)
with (root/'played.jsonl').open('a') as log:
    log.write(json.dumps({'frames':frames,'peak':max(abs(v) for v in values),'time':time.monotonic()})+'\\n')
time.sleep(.15)
''')
    player.chmod(0o755)
    (root/'played.jsonl').write_text('')
env={k:v for k,v in os.environ.items() if not k.startswith('FEATHER_DEBUG_')}
env.pop('WAYLAND_DEBUG',None)
env.update(WAYLAND_DISPLAY=a.host,PATH=str(fixture)+os.pathsep+env['PATH'],
           FEATHER_FAKE_AUDIO=str(root),DBUS_SYSTEM_BUS_ADDRESS='unix:path='+str(root/'no-system-bus'),PULSE_SERVER='unix:'+str(root/'never-real-pulse'),
           XDG_CONFIG_HOME=str(root/'config'),XDG_DATA_HOME=str(root/'data'),
           FEATHER_SCALE=a.scale,FEATHER_XWAYLAND='0',FEATHER_PROFILE='1',FEATHER_PROFILE_UI='1',
           FEATHER_DEBUG_INPUT_ONLY='1',FEATHER_DEBUG_AUDIO_KEYS='1',RUST_LOG='feather_de=debug,smithay=warn')
if a.microphone:
    env.pop('FEATHER_DEBUG_AUDIO_KEYS',None)
    env['FEATHER_DEBUG_AUDIO_SETTINGS']='1'
if a.system_sounds:
    env.pop('FEATHER_DEBUG_AUDIO_KEYS',None)
    env.update(FEATHER_DEBUG_NOTIFY='1',XDG_DATA_DIRS=str(root/'empty-data'))
    env['FEATHER_DEBUG_SYSTEM_SOUNDS_RESTORE' if a.restore_sounds else 'FEATHER_DEBUG_SYSTEM_SOUNDS']='1'
if a.protocol_summary:env['WAYLAND_DEBUG']='client'
if a.screenshot:env.update(FEATHER_DEBUG_SCREENSHOT=str(root/'frame.png'),FEATHER_DEBUG_SCREENSHOT_MS='9500' if a.system_sounds else '6900' if a.microphone else '4550')
(root/'pid').unlink(missing_ok=True)
terminal=Path('target/release/feather-terminal').resolve()
if a.system_sounds and not a.restore_sounds:
    client=root/'bell.py'
    client.write_text("import time\ntime.sleep(7.8)\nprint('\\a',flush=True)\ntime.sleep(100)\n")
    notify=root/'notify.sh'
    notify.write_text('#!/bin/sh\nsleep 7.1\nexec notify-send Test Sound\n')
command=['dbus-run-session' ,'--','sh','-c','echo $$ > "$1"; exec "$2" --windowed',
         'audio-key-check',str(root/'pid'),str(Path('target/release/feather-de').resolve())]
if a.system_sounds and not a.restore_sounds:
    import shlex
    # $3 is passed directly to exec; all clients inherit this private D-Bus/Wayland session.
    command[4]='echo $$ > "$1"; exec "$2" --windowed --autostart "$3"'
    command.append('sh '+shlex.quote(str(notify))+' & exec '+shlex.quote(str(terminal))+' -e python3 '+shlex.quote(str(client)))

def read_sample(pid):
    proc=Path(f'/proc/{pid}')
    stat=(proc/'stat').read_text().rsplit(')',1)[1].split()
    status=(proc/'status').read_text().splitlines()
    return {'ticks':int(stat[11])+int(stat[12]),'anon_kib':next(int(l.split()[1]) for l in status if l.startswith('RssAnon:'))}

def mutate(callback):
    with (root/'audio.lock').open('a') as lock:
        fcntl.flock(lock,fcntl.LOCK_EX)
        state=json.loads(statefile.read_text());callback(state);statefile.write_text(json.dumps(state))
    fd=os.open(fifo,os.O_WRONLY|os.O_NONBLOCK)
    try:os.write(fd,b'source\n' if a.microphone else b'server\n')
    finally:os.close(fd)

with (root/'compositor.log').open('w') as log:
    protocol=Counter();protocol_lock=threading.Lock();protocol_samples=[]
    process=subprocess.Popen(command,env=env,stdout=log,stderr=subprocess.PIPE if a.protocol_summary else subprocess.STDOUT,start_new_session=True)
    def collect_stderr():
        # Never retain keycodes, titles, text, paths or any other protocol arguments.
        for data in iter(process.stderr.readline,b''):
            line=data.decode('utf-8',errors='replace')
            match=re.search(r'\b([a-z][a-z0-9_]+)(?:@|#)\d+\.([A-Za-z0-9_]+)\(',line)
            if match:
                with protocol_lock:protocol['.'.join(match.groups())]+=1
            elif '[feather-' in line or 'panicked' in line or 'assertion' in line:
                log.write(line);log.flush()
    reader=threading.Thread(target=collect_stderr,daemon=True) if a.protocol_summary else None
    if reader:reader.start()

    start=time.monotonic();samples=[];thread_samples=[];last_second=-1;switched=False;removed=False;failed=False;restored=False
    try:
        while time.monotonic()-start<a.duration:
            elapsed=time.monotonic()-start
            if process.poll() is not None:raise RuntimeError((root/'compositor.log').read_text()[-7000:])
            if a.system_sounds:pass
            elif a.microphone:
                if elapsed>=6 and not switched:
                    def external(state):
                        state['default_source']='builtin mic'
                        state['reject_mute']=True
                        for v in state['sources'][0]['volume'].values():v['value']=16384
                    mutate(external);switched=True
                if elapsed>=7.2 and not removed:mutate(lambda s:s.update(sources=[]));removed=True
                if elapsed>=8.4 and not failed:mutate(lambda s:s.update(input_error=True));failed=True
                if elapsed>=9.6 and not restored:mutate(lambda s:s.update(sources=sources,default_source='usb mic',input_error=False));restored=True
            else:
                if elapsed>=7 and not switched:mutate(lambda s:s.update(default='usb output'));switched=True
                if elapsed>=8.3 and not removed:mutate(lambda s:s.update(sinks=[]));removed=True
            if (root/'pid').exists():
                row=read_sample(int((root/'pid').read_text()));row['seconds']=round(elapsed,3);samples.append(row)
                if int(elapsed)!=last_second:
                    last_second=int(elapsed);threads={}
                    if a.protocol_summary:
                        with protocol_lock:protocol_samples.append({'seconds':round(elapsed,3),'counts':dict(protocol)});protocol.clear()
                    for task in Path(f"/proc/{(root/'pid').read_text().strip()}/task").iterdir():
                        try:
                            stat=(task/'stat').read_text().rsplit(')',1)[1].split()
                            threads[task.name]={'name':(task/'comm').read_text().strip(),'ticks':int(stat[11])+int(stat[12])}
                        except FileNotFoundError:pass
                    thread_samples.append({'seconds':round(elapsed,3),'threads':threads})

            time.sleep(0.05)
        text=(root/'compositor.log').read_text()
        assert ('[feather-system-sounds-check] restored: ok' if a.restore_sounds else '[feather-system-sounds-check] complete: ok' if a.system_sounds else '[feather-microphone-check] complete: ok' if a.microphone else '[feather-audio-key-check] complete: ok') in text,text[-7000:]
        calls=[json.loads(l) for l in (root/'calls.jsonl').read_text().splitlines()]
        if a.system_sounds:
            played=[json.loads(l) for l in (root/'played.jsonl').read_text().splitlines()]
            if a.restore_sounds:assert not played,'restoring must not play a sound'
            else:
                assert [r['frames'] for r in played]==[15360,15360,15360,4320,3120,15360,10560,15360],played
                assert played[0]['peak']<played[1]['peak'],'volume must scale the PCM'
                assert not calls,'sound settings must never change hardware volume'
            print('System sound samples:',played)
        elif a.microphone:
            assert ['set-default-source','usb mic'] in calls
            assert ['set-source-volume','usb mic','50.000%'] in calls
            assert ['set-source-mute','usb mic','1'] in calls
            assert ['move-source-output','10','usb mic'] in calls
            assert not any(c[0].startswith('set-sink') or (c[0]=='move-source-output' and c[1]=='11') for c in calls)
        else:
            writes=[c for c in calls if c[0]=='set-sink-volume']
            assert writes and any(c[1]=='usb output' for c in writes)
            assert not any(c[0]=='set-sink-mute' and c[1]=='usb output' for c in calls)
            for c in writes:
                values=[float(v[:-1]) for v in c[2:]]
                assert all(0<=v<=100 for v in values)
                if c[1]=='built-in output':assert abs(values[0]*2-values[1])<0.03,'channel balance'
        end=samples[-1];idle=max((s for s in samples if s['seconds']<=end['seconds']-5),key=lambda s:s['seconds'])
        if a.protocol_summary:
            with protocol_lock:protocol_samples.append({'seconds':round(time.monotonic()-start,3),'counts':dict(protocol)});protocol.clear()
        host_input=sum(count for row in protocol_samples if row['seconds']>=idle['seconds']
                       for method,count in row['counts'].items()
                       if method.startswith(('wl_pointer.','wl_keyboard.','wl_touch.')))
        report={'pid':int((root/'pid').read_text()),'scale':a.scale,'microphone':a.microphone,'system_sounds':a.system_sounds,'peak_anon_kib':max(s['anon_kib'] for s in samples),
                'steady_anon_kib':end['anon_kib'],'idle_ticks_5s':end['ticks']-idle['ticks'],
                'idle_window_seconds':round(end['seconds']-idle['seconds'],3),
                'idle_host_input_events':host_input if a.protocol_summary else None,
                'sound_writes':len(calls),'screenshot':(root/'frame.png').exists(),'samples':samples,'thread_samples':thread_samples,'protocol_samples':protocol_samples}
        (root/'report.json').write_text(json.dumps(report,indent=2))
        print(json.dumps({k:v for k,v in report.items() if k not in ['samples','thread_samples','protocol_samples']}))
        assert host_input==0,'host input contaminated the idle measurement'
        assert report['idle_ticks_5s']==0
        if not a.screenshot:assert report['peak_anon_kib']*1024<=60_000_000
        else:
            import struct
            width,height=struct.unpack('>II',(root/'frame.png').read_bytes()[16:24])
            assert report['peak_anon_kib']*1024<=60_000_000+2*width*height*4,'readback peak budget'
            peak_sample=max(samples,key=lambda s:s['anon_kib'])
            assert any(peak_sample['seconds']<=s['seconds']<=peak_sample['seconds']+2 and s['anon_kib']*1024<=60_000_000 for s in samples),'readback memory must return within 2 seconds'
    finally:
        if process.poll() is None:
            os.killpg(process.pid,signal.SIGTERM)
            try:process.wait(timeout=4)
            except subprocess.TimeoutExpired:os.killpg(process.pid,signal.SIGKILL);process.wait()
        if reader:reader.join(timeout=4)
