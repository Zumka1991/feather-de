#!/usr/bin/env python3
"""Real ext-session-lock clients + native lock checks in owned nested compositors.

No host input, PAM password, logind call or session restart. Only own process groups stop.
The small wire client also tests rejected lock objects that try unlock_and_destroy.
"""
import argparse
import array
import json
import os
from pathlib import Path
import shlex
import signal
import socket
import struct
import subprocess
import tempfile
import time

p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--output',type=Path)
p.add_argument('--scale',default='1.25')
p.add_argument('--no-screenshot',action='store_true')
a=p.parse_args()
root=(a.output or Path(tempfile.mkdtemp(prefix='feather-lock-'))).resolve()
root.mkdir(parents=True,exist_ok=True)

def uint(*n):return struct.pack('<'+'I'*len(n),*n)
def string(s):
    b=s.encode()+b'\0'
    return uint(len(b))+b+b'\0'*((-len(b))%4)

class Client:
    def __init__(self,path):
        self.sock=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM)
        self.sock.settimeout(5)
        self.sock.connect(str(path))
        self.buf=b'';self.events=[];self.globals={};self.next_id=4
        self.send(1,1,uint(2))
        self.send(1,0,uint(3))
        self.until(lambda e:e[0]==3)
        self.manager=self.bind('ext_session_lock_manager_v1',1)
    def new(self):
        i=self.next_id;self.next_id+=1;return i
    def send(self,obj,opcode,payload=b'',fd=None):
        b=uint(obj,((len(payload)+8)<<16)|opcode)+payload
        if fd is None:self.sock.sendall(b)
        else:self.sock.sendmsg([b],[(socket.SOL_SOCKET,socket.SCM_RIGHTS,array.array('i',[fd]))])
    def event(self):
        while len(self.buf)<8:self.buf+=self.read()
        obj,header=struct.unpack_from('<II',self.buf)
        size=header>>16
        assert size>=8
        while len(self.buf)<size:self.buf+=self.read()
        payload=self.buf[8:size];self.buf=self.buf[size:]
        e=(obj,header&65535,payload);self.events.append(e)
        if obj==2 and e[1]==0:
            name,n=struct.unpack_from('<II',payload)
            interface=payload[8:8+n-1].decode()
            version=struct.unpack_from('<I',payload,8+((n+3)//4)*4)[0]
            self.globals[interface]=(name,version)
        return e
    def read(self):
        b=self.sock.recv(65536)
        assert b,'unexpected client disconnection'
        return b
    def until(self,predicate):
        for _ in range(200):
            e=self.event()
            if predicate(e):return e
            if e[0]==1 and e[1]==0:raise AssertionError(f'Wayland error: {e}')
        raise AssertionError('event not received')
    def bind(self,interface,version):
        name,available=self.globals[interface];i=self.new()
        self.send(2,0,uint(name)+string(interface)+uint(min(version,available),i))
        return i
    def lock(self):
        i=self.new();self.send(self.manager,1,uint(i));return i
    def picture(self,lock):
        compositor=self.bind('wl_compositor',4);output=self.bind('wl_output',1)
        shm=self.bind('wl_shm',1);seat=self.bind('wl_seat',1)
        keyboard=self.new();self.send(seat,1,uint(keyboard))
        pointer=self.new();self.send(seat,0,uint(pointer))
        surface=self.new();self.send(compositor,0,uint(surface))
        lock_surface=self.new();self.send(lock,1,uint(lock_surface,surface,output))
        e=self.until(lambda e:e[0]==lock_surface and e[1]==0)
        serial,w,h=struct.unpack('<III',e[2]);assert w>0 and h>0
        self.send(lock_surface,1,uint(serial))
        fd=os.memfd_create('owned-lock-fixture',os.MFD_CLOEXEC)
        try:
            os.ftruncate(fd,w*h*4)
            pixel=b'\x50\x90\x20\xff' # XRGB8888, opaque green
            data=pixel*w*h
            offset=0
            while offset<len(data):offset+=os.write(fd,data[offset:])
            pool=self.new();self.send(shm,0,uint(pool,w*h*4),fd)
            buffer=self.new();self.send(pool,0,uint(buffer,0,w,h,w*4,1))
            self.send(pool,1)
            self.send(surface,1,uint(buffer,0,0));self.send(surface,2,uint(0,0,w,h));self.send(surface,6)
            self.until(lambda e:e[0]==keyboard and e[1]==1 and struct.unpack_from('<I',e[2],4)[0]==surface)
        finally:os.close(fd)
        return w,h,surface,seat,pointer
    def close(self):self.sock.close()

def sample(pid):
    stat=Path(f'/proc/{pid}/stat').read_text().rsplit(')',1)[1].split()
    status=Path(f'/proc/{pid}/status').read_text().splitlines()
    return {'ticks':int(stat[11])+int(stat[12]),'anon_kib':int(next(s for s in status if s.startswith('RssAnon:')).split()[1])}

reports=[]
for mode in ['protocol','native']:
    folder=root/mode;folder.mkdir(exist_ok=True)
    env=os.environ.copy()
    for k in list(env):
        if k.startswith('FEATHER_DEBUG_'):env.pop(k)
    env.update(XDG_CONFIG_HOME=str(folder/'config'),XDG_DATA_HOME=str(folder/'data'),FEATHER_SCALE=a.scale,
               FEATHER_DEBUG_INPUT_ONLY='1',FEATHER_PROFILE='1',RUST_LOG='feather_de=debug,smithay=warn')
    config=folder/'config/feather-de';config.mkdir(parents=True,exist_ok=True)
    (config/'settings.conf').write_text('show_about=false\n')
    if mode=='native':env['FEATHER_DEBUG_LOCK_CHECK']='1'
    else:env['FEATHER_DEBUG_EXTERNAL_LOCK_CHECK']='1'
    if not a.no_screenshot:env.update(FEATHER_DEBUG_SCREENSHOT=str(folder/'frame.png'),FEATHER_DEBUG_SCREENSHOT_MS='6500')
    socket_file=folder/'socket'
    socket_file.unlink(missing_ok=True)
    script='printf %s "$WAYLAND_DISPLAY" > '+shlex.quote(str(socket_file))
    if mode=='native':script+='; exec '+shlex.quote(str(Path('target/release/feather-terminal').resolve()))
    with (folder/'compositor.log').open('w') as log:
        proc=subprocess.Popen(['target/release/feather-de','--windowed','--autostart',script],env=env,stdout=log,stderr=log,start_new_session=True)
        clients=[]
        try:
            for _ in range(80):
                time.sleep(.1)
                assert proc.poll() is None,'owned compositor exited'
                if socket_file.exists() and socket_file.read_text():break
            path=Path(env['XDG_RUNTIME_DIR'])/socket_file.read_text()
            if mode=='protocol':
                owner=Client(path);clients.append(owner)
                for interface in ['ext_idle_notifier_v1','zwp_idle_inhibit_manager_v1']:assert interface in owner.globals
                lock=owner.lock();owner.until(lambda e:e[0]==lock and e[1]==0)
                w,h,surface,seat,pointer=owner.picture(lock)
                owner.until(lambda e:e[0]==pointer and e[1]==3 and struct.unpack_from('<I',e[2],12)[0]==1)
                owner.until(lambda e:e[0]==pointer and e[1]==2)
                owner.until(lambda e:e[0]==pointer and e[1]==3 and struct.unpack_from('<I',e[2],12)[0]==0)
                inhibition=owner.bind('zwp_idle_inhibit_manager_v1',1)
                inhibitor=owner.new();owner.send(inhibition,1,uint(inhibitor,surface))
                idle=owner.bind('ext_idle_notifier_v1',1)
                notification=owner.new();owner.send(idle,1,uint(notification,50,seat))
                owner.until(lambda e:e[0]==notification and e[1]==0)
                # A lock surface must not inhibit the locked seat's idle notifications.
                attacker=Client(path);clients.append(attacker)
                rejected=attacker.lock();attacker.until(lambda e:e[0]==rejected and e[1]==1)
                attacker.send(rejected,2) # must never invoke unlock for a rejected lock
                error=attacker.until(lambda e:e[0]==1 and e[1]==0)
                assert error
                observer=Client(path);clients.append(observer)
                rejected=observer.lock();observer.until(lambda e:e[0]==rejected and e[1]==1)
                observer.send(rejected,0);observer.close();clients.remove(observer)
                owner.send(lock,2) # accepted owner can unlock
                successor=Client(path);clients.append(successor)
                lock2=successor.lock();successor.until(lambda e:e[0]==lock2 and e[1]==0)
                successor.close();clients.remove(successor) # fail closed; native panel must appear
                probe=Client(path);clients.append(probe)
                rejected=probe.lock();probe.until(lambda e:e[0]==rejected and e[1]==1)
                probe.send(rejected,0)
            samples=[]
            for _ in range(17):
                time.sleep(1);assert proc.poll() is None,'owned compositor exited'
                samples.append(sample(proc.pid))
            text=(folder/'compositor.log').read_text()
            if mode=='native':assert text.count('[feather-lock-check]')==2,text
            assert 'panicked' not in text,text
            report={'mode':mode,'idle_ticks_5s':samples[-1]['ticks']-samples[-6]['ticks'],
                    'peak_anon_kib':max(s['anon_kib'] for s in samples),'samples':samples}
            reports.append(report)
            print({k:v for k,v in report.items() if k!='samples'},flush=True)
        finally:
            for client in clients:client.close()
            try:os.killpg(proc.pid,signal.SIGTERM)
            except ProcessLookupError:pass
            try:proc.wait(timeout=3)
            except subprocess.TimeoutExpired:os.killpg(proc.pid,signal.SIGKILL);proc.wait()
(root/'report.json').write_text(json.dumps(reports,indent=2))
print(root)
