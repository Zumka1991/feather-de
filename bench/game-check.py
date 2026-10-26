#!/usr/bin/env python3
"""Game input protocol checks on a private D-Bus and an owned nested compositor."""
import argparse, json, os, shlex, signal, subprocess, tempfile, time
from pathlib import Path
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--mode',choices=['pointer','x11'],default='pointer')
p.add_argument('--compositor',default='target/release/feather-de')
p.add_argument('--output',type=Path)
a=p.parse_args(); root=(a.output or Path(tempfile.mkdtemp(prefix='feather-game-'))).resolve(); root.mkdir(parents=True,exist_ok=True)
protocols=Path(subprocess.check_output(['pkg-config','--variable=pkgdatadir','wayland-protocols'],text=True).strip())
generated=[]
for name,path in [('xdg-shell','stable/xdg-shell/xdg-shell.xml'),('pointer-constraints-unstable-v1','unstable/pointer-constraints/pointer-constraints-unstable-v1.xml'),('relative-pointer-unstable-v1','unstable/relative-pointer/relative-pointer-unstable-v1.xml')]:
 for mode,ext in [('client-header','-client-protocol.h'),('private-code','-protocol.c')]:
  out=root/(name+ext);subprocess.run(['wayland-scanner',mode,str(protocols/path),str(out)],check=True)
  if ext.endswith('.c'):generated.append(str(out))
fixture=root/'game-pointer-fixture'
flags=shlex.split(subprocess.check_output(['pkg-config','--cflags','--libs','wayland-client'],text=True))
subprocess.run(['cc','-Wall','-Wextra','-Werror','-I'+str(root),'bench/game-pointer.c',*generated,'-o',str(fixture),*flags],check=True)
if a.mode=='x11':
 fixture=root/'game-x11-fixture'
 subprocess.run(['cc','-Wall','-Wextra','-Werror','bench/game-x11.c','-lxcb','-o',str(fixture)],check=True)
config=root/'config/feather-de';config.mkdir(parents=True,exist_ok=True);(config/'settings.conf').write_text('show_about=false\nfirst_run_completed=true\n')
env={k:v for k,v in os.environ.items() if not k.startswith('FEATHER_DEBUG_')}
env.update(XDG_CONFIG_HOME=str(root/'config'),XDG_DATA_HOME=str(root/'data'),FEATHER_PROFILE='1',FEATHER_SCALE='2',FEATHER_XWAYLAND='0',FEATHER_DEBUG_INPUT_ONLY='1',FEATHER_DEBUG_GAME_POINTER='1',RUST_LOG='feather_de=debug,smithay=warn')
if a.mode=='x11':
 env['FEATHER_XWAYLAND']='1';env.pop('FEATHER_DEBUG_GAME_POINTER');env['FEATHER_DEBUG_GAME_X11']='1'
command=[str(Path(a.compositor).resolve()),'--windowed','--autostart',f'exec {shlex.quote(str(fixture))} >{shlex.quote(str(root/"client.log"))} 2>&1']
script=f'echo $$ > {shlex.quote(str(root/"compositor.pid"))}; exec '+shlex.join(command)
def sample(pid):
 stat=Path(f'/proc/{pid}/stat').read_text().rsplit(')',1)[1].split();assert stat[0]!='Z'
 status=Path(f'/proc/{pid}/status').read_text().splitlines()
 return {'ticks':int(stat[11])+int(stat[12]),'anon_kib':int(next(s for s in status if s.startswith('RssAnon:')).split()[1])}
with (root/'session.log').open('w') as log:
 proc=subprocess.Popen(['dbus-run-session','--','sh','-c',script],env=env,stdout=log,stderr=log,start_new_session=True)
 try:
  samples=[]
  for second in range(1,13):
   time.sleep(1);assert proc.poll() is None,f'compositor exited; {root}/session.log'
   pid=int((root/'compositor.pid').read_text());samples.append({'second':second,**sample(pid)})
  log.flush();text=(root/'session.log').read_text();client=(root/'client.log').read_text()
  if a.mode=='pointer':
   assert '[feather-game-check]' in text,text[-3000:]
  else:
   assert '[feather-game-x11-check]' in text,text[-3000:]
   assert client.count('button 1 1 200 100\n')==2,client
   assert client.count('button 1 0 200 100\n')==2,client
   assert 'size 760 440\n' in client,client
  if a.mode=='pointer':
   relative=[line for line in client.splitlines() if line.startswith('relative ')]
   assert relative==['relative 1 0 1000000 3.000 -2.000 7.000 -5.000','relative 2 0 1000100 -5.000 1.000 -9.000 4.000'],relative
   assert client.count('button 273 1\n')==3,client
   assert client.count('button 273 0\n')==3,client
   assert client.count('button 274 1\n')==1 and client.count('button 274 0\n')==1,client
  report={'compositor_pid':pid,'idle_ticks_5s':samples[-1]['ticks']-samples[-6]['ticks'],'peak_sampled_anon_kib':max(s['anon_kib'] for s in samples),'samples':samples}
  (root/'report.json').write_text(json.dumps(report,indent=2));print({k:v for k,v in report.items() if k!='samples'});print(root)
 finally:
  try:os.killpg(proc.pid,signal.SIGTERM)
  except ProcessLookupError:pass
  try:proc.wait(timeout=3)
  except subprocess.TimeoutExpired:os.killpg(proc.pid,signal.SIGKILL);proc.wait()
