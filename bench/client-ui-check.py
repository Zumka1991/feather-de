#!/usr/bin/env python3
"""Only owned nested compositor/clients, private data/config, no host input.

python3 bench/client-ui-check.py files --output /tmp/feather-files-ui
python3 bench/client-ui-check.py notes --output /tmp/feather-notes-ui
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

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('client', choices=['files', 'notes'])
p.add_argument('--output', type=Path)
p.add_argument('--no-screenshot', action='store_true')
p.add_argument('--scale', default='1.25')
a = p.parse_args()
root = (a.output or Path(tempfile.mkdtemp(prefix='feather-client-ui-'))).resolve()
root.mkdir(parents=True, exist_ok=True)
config = root/'config/feather-de/settings.conf'
config.parent.mkdir(parents=True, exist_ok=True)
def save(appearance):
    temporary = config.with_suffix('.tmp')
    temporary.write_text('appearance='+appearance+'\nshow_about=false\ndock=builtin.launchpad,dolphin\n')
    temporary.replace(config)
save('light')
fixture = root/'fixture'
fixture.mkdir(exist_ok=True)
picture = fixture/'Горное озеро.png'
picture.write_bytes(Path('assets/wallpapers/feather-lake.png').read_bytes())
(fixture/'Прочитай меня.txt').write_text('Быстрый просмотр\nПёрышко — лёгкое окружение рабочего стола.\n')
notes = root/'data/feather-notes'
notes.mkdir(parents=True, exist_ok=True)
(notes/'1.txt').write_text('Планы на день\nПроверить тёмную тему\nПосмотреть фотографии\n')
env = os.environ.copy()
for key in list(env):
    if key.startswith('FEATHER_DEBUG_') or key.startswith('FEATHER_FILES_DEBUG_'): env.pop(key)
env.update(XDG_CONFIG_HOME=str(root/'config'), XDG_DATA_HOME=str(root/'data'),
           FEATHER_SCALE=a.scale, FEATHER_PROFILE='1', FEATHER_DEBUG_INPUT_ONLY='1',
           FEATHER_CLIENT_UI_CHECK='1')
if a.client == 'files': env['FEATHER_FILES_DEBUG_PREVIEW'] = str(picture)
if not a.no_screenshot:
    env.update(FEATHER_DEBUG_SCREENSHOT=str(root/'frame.png'), FEATHER_DEBUG_SCREENSHOT_MS='8000')
client = [str(Path('target/release/feather-'+a.client).resolve())]
if a.client == 'files': client.append(str(fixture))
client_script = ('echo $$ > '+shlex.quote(str(root/'client.pid'))+'; exec '+shlex.join(client)
                 +' 2>'+shlex.quote(str(root/'client.log')))
command = ['target/release/feather-de', '--windowed', '--autostart', client_script]
def sample(pid):
    values = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
    assert values[0] != 'Z', 'owned process died'
    status = Path(f'/proc/{pid}/status').read_text().splitlines()
    return {'ticks':int(values[11])+int(values[12]), 'anon_kib':int(next(s for s in status if s.startswith('RssAnon:')).split()[1])}
with (root/'compositor.log').open('w') as log:
    proc = subprocess.Popen(command, env=env, stdout=log, stderr=log, start_new_session=True)
    try:
        samples = []
        client_pid = None
        for second in range(1, 19):
            time.sleep(1)
            assert proc.poll() is None, 'owned compositor exited'
            if (root/'client.pid').exists(): client_pid = int((root/'client.pid').read_text())
            assert client_pid, 'owned client did not start'
            samples.append({'second':second, 'compositor':sample(proc.pid), 'client':sample(client_pid)})
            if second in (2, 4, 6): save('light' if second == 4 else 'dark')
        text = (root/'client.log').read_text()
        assert text.count('appearance=Light') == 2, text
        assert text.count('appearance=Dark') == 2, text
        if a.client == 'files': assert 'preview-ready generation=1 ok=true' in text, text
        report = {'client':a.client, 'scale':a.scale, 'compositor_pid':proc.pid, 'client_pid':client_pid,
                  'compositor_idle_ticks_5s':samples[-1]['compositor']['ticks']-samples[-6]['compositor']['ticks'],
                  'client_idle_ticks_5s':samples[-1]['client']['ticks']-samples[-6]['client']['ticks'],
                  'peak_compositor_anon_kib':max(s['compositor']['anon_kib'] for s in samples),
                  'peak_client_anon_kib':max(s['client']['anon_kib'] for s in samples), 'samples':samples}
        (root/'report.json').write_text(json.dumps(report, indent=2))
        print({k:v for k,v in report.items() if k!='samples'}, flush=True)
        print(root, flush=True)
    finally:
        try: os.killpg(proc.pid, signal.SIGTERM)
        except ProcessLookupError: pass
        try: proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGKILL)
            proc.wait()
