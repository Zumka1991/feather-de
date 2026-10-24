#!/usr/bin/env python3
"""Virtual output hotplug + real Wayland clients in an owned nested compositor.

No host input, DRM modeset, PAM authentication or real audio changes. Only the owned
process group is stopped. --host must name a socket whose owner was checked first.
"""
import argparse
import array
import json
import os
from pathlib import Path
import select
import shlex
import signal
import socket
import struct
import subprocess
import tempfile
import time


def uint(*n): return struct.pack('<'+'I'*len(n), *n)
def string(s):
    b=s.encode()+b'\0'
    return uint(len(b))+b+b'\0'*((-len(b))%4)

class Wire:
    def __init__(self,path):
        self.sock=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM); self.sock.connect(str(path))
        self.buf=b''; self.events=[]; self.globals={}; self.outputs={}; self.removed=[]; self.next_id=3
        self.send(1,1,uint(2)); self.sync()
    def new(self):
        n=self.next_id; self.next_id+=1; return n
    def send(self,obj,opcode,payload=b'',fd=None):
        packet=uint(obj,((len(payload)+8)<<16)|opcode)+payload
        if fd is None: self.sock.sendall(packet)
        else: self.sock.sendmsg([packet],[(socket.SOL_SOCKET,socket.SCM_RIGHTS,array.array('i',[fd]))])
    def bind(self,interface,version=1,global_name=None):
        name=global_name if global_name is not None else next(n for n,(i,_) in self.globals.items() if i==interface)
        i,available=self.globals[name]; obj=self.new()
        self.send(2,0,uint(name)+string(i)+uint(min(version,available),obj)); return obj
    def sync(self):
        obj=self.new(); self.send(1,0,uint(obj)); self.until(lambda e:e[0]==obj)
    def pump(self,timeout=0):
        if select.select([self.sock],[],[],timeout)[0]:
            data=self.sock.recv(65536); assert data,'Wayland client disconnected'; self.buf+=data
        while len(self.buf)>=8:
            obj,header=struct.unpack_from('<II',self.buf); size=header>>16; assert size>=8
            if len(self.buf)<size: break
            payload=self.buf[8:size]; self.buf=self.buf[size:]; event=(obj,header&65535,payload)
            self.events.append(event)
            if obj==1 and event[1]==0: raise AssertionError('Wayland protocol error: '+repr(payload))
            if obj==2 and event[1]==0:
                name,n=struct.unpack_from('<II',payload); interface=payload[8:8+n-1].decode()
                version=struct.unpack_from('<I',payload,8+((n+3)//4)*4)[0]; self.globals[name]=(interface,version)
                if interface=='wl_output':
                    output=self.bind(interface,4,name); self.outputs[output]={'global':name}
            elif obj==2 and event[1]==1:
                name=struct.unpack('<I',payload)[0]; self.removed.append(self.globals.pop(name,None))
            elif obj in self.outputs:
                if event[1]==0: self.outputs[obj]['position']=struct.unpack_from('<ii',payload)
                elif event[1]==1: self.outputs[obj]['mode']=struct.unpack_from('<Iiii',payload)
                elif event[1]==4:
                    n=struct.unpack_from('<I',payload)[0]; self.outputs[obj]['name']=payload[4:4+n-1].decode()
    def until(self,predicate,timeout=5):
        start=len(self.events); deadline=time.monotonic()+timeout
        while time.monotonic()<deadline:
            self.pump(.05)
            for event in self.events[start:]:
                if predicate(event): return event
            start=len(self.events)
        raise AssertionError('expected Wayland event not received')
    def lock_pictures(self):
        manager=self.bind('ext_session_lock_manager_v1'); lock=self.new(); self.send(manager,1,uint(lock))
        self.until(lambda e:e[0]==lock and e[1]==0)
        compositor=self.bind('wl_compositor',4); shm=self.bind('wl_shm'); seat=self.bind('wl_seat',5)
        self.pointer=self.new(); self.send(seat,0,uint(self.pointer)); self.lock_surfaces=[]
        for index,output in enumerate(list(self.outputs)):
            if self.outputs[output]['global'] not in self.globals: continue
            surface=self.new(); self.send(compositor,0,uint(surface)); role=self.new(); self.send(lock,1,uint(role,surface,output))
            e=self.until(lambda e:e[0]==role and e[1]==0); serial,w,h=struct.unpack('<III',e[2]); assert w>=320 and h>=200
            self.send(role,1,uint(serial)); fd=os.memfd_create('owned-monitor-lock',os.MFD_CLOEXEC)
            try:
                os.ftruncate(fd,w*h*4); row=(b'\x20\xa0\x40\xff' if index==0 else b'\xc0\x60\x20\xff')*w
                for _ in range(h): os.write(fd,row)
                pool=self.new(); self.send(shm,0,uint(pool,w*h*4),fd); buffer=self.new()
                self.send(pool,0,uint(buffer,0,w,h,w*4,1)); self.send(pool,1)
                self.send(surface,1,uint(buffer,0,0)); self.send(surface,2,uint(0,0,w,h)); self.send(surface,6)
                self.lock_surfaces.append(surface)
            finally: os.close(fd)
        self.sync(); assert len(self.lock_surfaces)==2
    def close(self): self.sock.close()

p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--host',required=True); p.add_argument('--scale',default='1'); p.add_argument('--output',type=Path)
p.add_argument('--screenshot',action='store_true'); p.add_argument('--mode',choices=['native','external','restore','all'],default='all')
a=p.parse_args(); root=(a.output or Path(tempfile.mkdtemp(prefix='feather-monitors-'))).resolve(); root.mkdir(parents=True,exist_ok=True)
config=root/'config/feather-de'; config.mkdir(parents=True,exist_ok=True)
if a.mode!='restore': (config/'settings.conf').write_text('show_about=false\n')
reports=[]
def sample(pid):
    proc=Path(f'/proc/{pid}'); fields=(proc/'stat').read_text().rsplit(')',1)[1].split()
    return {'ticks':int(fields[11])+int(fields[12]),'anon_kib':next(int(l.split()[1]) for l in (proc/'status').read_text().splitlines() if l.startswith('RssAnon:'))}

def run(mode):
    folder=root/mode; folder.mkdir(exist_ok=True); socket_file=folder/'socket'; socket_file.unlink(missing_ok=True)
    env={k:v for k,v in os.environ.items() if not k.startswith('FEATHER_DEBUG_')}; env.pop('WAYLAND_DEBUG',None)
    env.update(WAYLAND_DISPLAY=a.host,XDG_CONFIG_HOME=str(root/'config'),XDG_DATA_HOME=str(root/'data'),
               DBUS_SYSTEM_BUS_ADDRESS='unix:path='+str(root/'no-system-bus'),PULSE_SERVER='unix:'+str(root/'no-audio'),
               FEATHER_SCALE=a.scale,FEATHER_DEBUG_INPUT_ONLY='1',FEATHER_DEBUG_MONITORS=mode,FEATHER_XWAYLAND='0',FEATHER_PROFILE='1',FEATHER_PROFILE_UI='1')
    if mode=='external':
        private=folder/'config/feather-de'; private.mkdir(parents=True,exist_ok=True); (private/'settings.conf').write_text('show_about=false\n'); env['XDG_CONFIG_HOME']=str(folder/'config')
    if a.screenshot: env['FEATHER_DEBUG_MONITOR_DIR']=str(folder)
    launch='printf %s "$WAYLAND_DISPLAY" > '+shlex.quote(str(socket_file))
    if mode=='native':
        launch+='; exec env WAYLAND_DEBUG=client '+shlex.quote(str(Path('target/release/feather-terminal').resolve()))+' -e /usr/bin/sleep 60 2>'+shlex.quote(str(folder/'client.log'))
    binary=str(Path('target/release/feather-de').resolve()); pidfile=folder/'pid'; pidfile.unlink(missing_ok=True)
    command=['dbus-run-session','--','sh','-c','echo $$ > "$1"; exec "$2" --windowed --autostart "$3"','monitor-check',str(pidfile),binary,launch]
    duration=24 if mode=='native' else 13 if mode=='external' else 15
    clients=[]; observer=None; owner=None; samples=[]; owner_closed=False
    with (folder/'compositor.log').open('w') as log:
        proc=subprocess.Popen(command,env=env,stdout=log,stderr=subprocess.STDOUT,start_new_session=True); start=time.monotonic()
        try:
            while time.monotonic()-start<duration:
                elapsed=time.monotonic()-start
                assert proc.poll() is None,(folder/'compositor.log').read_text()[-6000:]
                if pidfile.exists():
                    row=sample(int(pidfile.read_text())); row['seconds']=round(elapsed,3); samples.append(row)
                if elapsed>.8 and observer is None and socket_file.exists():
                    path=Path(env['XDG_RUNTIME_DIR'])/socket_file.read_text(); observer=Wire(path); clients.append(observer); observer.sync()
                    assert len(observer.outputs)==2,observer.outputs
                    if mode=='external':
                        owner=Wire(path); clients.append(owner); owner.lock_pictures()
                for client in clients: client.pump()
                if mode=='external' and elapsed>=3.5 and not owner_closed:
                    enters=[e for e in owner.events if e[0]==owner.pointer and e[1]==0]
                    motions=[e for e in owner.events if e[0]==owner.pointer and e[1]==2]
                    buttons=[e for e in owner.events if e[0]==owner.pointer and e[1]==3]
                    assert any(struct.unpack_from('<I',e[2],4)[0]==owner.lock_surfaces[1] for e in enters),enters
                    assert any(struct.unpack_from('<ii',e[2],4)==(100*256,100*256) for e in motions),motions
                    assert len(buttons)>=2,buttons
                    owner.close(); clients.remove(owner); owner_closed=True
                time.sleep(.05)
            text=(folder/'compositor.log').read_text(); expected='restored' if mode=='restore' else 'complete'
            assert f'[feather-monitor-check] {expected}: ok' in text,text[-7000:]
            if mode=='native':
                assert len(observer.removed)>=2,observer.removed
                client_text=(folder/'client.log').read_text()
                assert '.enter(wl_output' in client_text and '.leave(wl_output' in client_text,'real client output membership'
            end=samples[-1]; idle=max((s for s in samples if s['seconds']<=end['seconds']-5),key=lambda s:s['seconds'])
            report={'mode':mode,'scale':a.scale,'pid':int(pidfile.read_text()),'peak_anon_kib':max(s['anon_kib'] for s in samples),
                    'steady_anon_kib':end['anon_kib'],'idle_ticks_5s':end['ticks']-idle['ticks'],
                    'idle_window_seconds':round(end['seconds']-idle['seconds'],3),'output_globals_removed':len(observer.removed),
                    'outputs':list(observer.outputs.values()),'samples':samples}
            (folder/'report.json').write_text(json.dumps(report,indent=2)); reports.append(report)
            print(json.dumps({k:v for k,v in report.items() if k not in ('samples','outputs')}),flush=True)
            assert report['idle_ticks_5s']==0,'idle CPU budget'
            if not a.screenshot: assert report['peak_anon_kib']*1024<=60_000_000,'private memory budget'
            else:
                from PIL import Image
                images={p.name:Image.open(p).convert('RGB') for p in folder.glob('*.png')}
                assert images,'compositor frames were not captured'
                for name in ['locked-secondary.png','locked-reconnected.png','external-fallback.png']:
                    if name in images: assert images[name].getextrema()==((0,0),(0,0),(0,0)),name
                if mode=='external': assert images['external-secondary.png'].getpixel((100,100))==(32,96,192)
                frame=max(im.width*im.height*4 for im in images.values())
                assert report['peak_anon_kib']*1024<=60_000_000+2*frame,'readback memory budget'
                for im in images.values(): im.close()
        finally:
            for client in clients: client.close()
            try: os.killpg(proc.pid,signal.SIGTERM)
            except ProcessLookupError: pass
            try: proc.wait(timeout=4)
            except subprocess.TimeoutExpired: os.killpg(proc.pid,signal.SIGKILL); proc.wait()

for mode in (['native','external','restore'] if a.mode=='all' else [a.mode]): run(mode)
(root/'report.json').write_text(json.dumps(reports,indent=2))
print(root)
