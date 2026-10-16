#!/usr/bin/env python3
"""Drive the real TUI on a deterministic disposable repo; retain logs/screens.
Usage: python3 scripts/performance_tui.py /absolute/path/to/keifu /tmp/results [--pixel]
Request timings include the debug server poll interval; draw timings are in log.
"""
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import socket
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time

binary = str(Path(sys.argv[1]).resolve())
out = Path(sys.argv[2]).resolve()
out.mkdir(parents=True, exist_ok=True)
(out/'trace.log').unlink(missing_ok=True)
pixel = '--pixel' in sys.argv[3:]

def git(repo, *args, **kwargs):
    return subprocess.run(['git', '-C', str(repo), *args], check=True, stdout=subprocess.PIPE, **kwargs)

with tempfile.TemporaryDirectory(prefix='keifu-live-') as temp:
    root = Path(temp)
    repo = root / 'repo'
    repo.mkdir()
    git(repo, 'init', '-q', '-b', 'main')
    data = bytearray()
    for i in range(1, 3001):
        msg = f'Commit {i:05}: update parser'
        data.extend(f'commit refs/heads/main\nmark :{i}\ncommitter Perf <perf@example.com> {1700000000+i} +0000\ndata {len(msg)}\n{msg}\n'.encode())
        if i == 1:
            data.extend(b'M 100644 inline src.rs\ndata 33\nfn main() { println!("hello"); }\n\n')
        data.extend(b'\n')
    for j in range(3):
        msg = f'Feature {j}'
        data.extend(f'commit refs/heads/feature-{j}\nmark :{3001+j}\ncommitter Feature <feature@example.com> {1700004000+j} +0000\ndata {len(msg)}\n{msg}\nfrom :{2000+j*100}\n\n'.encode())
    git(repo, 'fast-import', '--quiet', input=data)
    git(repo, 'reset', '--hard', '-q', 'main')
    cfg = root / 'config' / 'keifu'
    cfg.mkdir(parents=True)
    (cfg / 'config.toml').write_text('[ui]\ngraph_renderer = "' + ('pixel' if pixel else 'unicode') + '"\ntheme = "dark"\n[refresh]\nauto_fetch = false\n')
    env = dict(os.environ, XDG_CONFIG_HOME=str(root/'config'), XDG_CACHE_HOME=str(root/'cache'), TERM='xterm-256color', KEIFU_LOG='debug')
    if pixel: env['KEIFU_FORCE_PIXEL'] = 'kitty'
    # Reserve an ephemeral port, release immediately before launch.
    with socket.socket() as reserve:
        reserve.bind(('127.0.0.1', 0))
        port = reserve.getsockname()[1]
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 35, 120, 0, 0))
    started = time.perf_counter()
    proc = subprocess.Popen([binary, '--debug-listen', f'127.0.0.1:{port}', '--log-file', str(out/'trace.log')], cwd=repo, env=env, stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
    os.close(slave)
    stopping = threading.Event()
    def drain():
        with (out/'terminal.raw').open('wb') as capture:
            while not stopping.is_set():
                if select.select([master], [], [], .1)[0]:
                    try: capture.write(os.read(master, 65536))
                    except OSError: break
    thread = threading.Thread(target=drain)
    thread.start()
    try:
        deadline = time.monotonic() + 30
        while True:
            try:
                sock = socket.create_connection(('127.0.0.1',port), timeout=1)
                break
            except OSError:
                if time.monotonic() > deadline: raise RuntimeError('TUI failed to start')
                time.sleep(.02)
        sock.settimeout(20)
        stream = sock.makefile('rwb', buffering=0)
        timings = []
        def request(cmd, **kwargs):
            start = time.perf_counter()
            stream.write((json.dumps(dict(cmd=cmd, **kwargs))+'\n').encode())
            reply = json.loads(stream.readline())
            timings.append(dict(cmd=cmd, ms=(time.perf_counter()-start)*1000))
            assert reply.get('ok'), reply
            return reply
        def screen(name):
            value = request('dump', width=120, height=35)['screen']
            (out/f'{name}.txt').write_text(value)
            return value
        state = request('state')
        assert state['commit_count'] >= 500, state
        startup_ms = (time.perf_counter()-started)*1000
        screen('startup')
        for _ in range(8):
            request('keys', keys='G')
            time.sleep(.15)
        state = request('state')
        assert state['commit_count'] == 3003, state
        bottom = screen('bottom')
        assert 'Commit 00001' in bottom, bottom
        request('keys', keys='g')
        top = screen('top')
        assert 'Feature 2' in top, top
        before = request('state')['selected_index']
        request('keys', keys='<down>')
        after = request('state')['selected_index']
        assert after > before
        screen('moved')
        for _ in range(20):
            request('keys', keys='G')
            request('dump', width=120, height=35)
            request('keys', keys='g')
            request('dump', width=120, height=35)
        # Mouse selection must agree with the displayed viewport after jumps.
        request('keys', keys='g')
        screen('mouse-before')
        request('mouse', kind='click', x=65, y=3)
        assert request('state')['selected_index'] != 0
        screen('mouse-after')
        # Open the actual root-file diff and verify content, including resize.
        request('keys', keys='G')
        deadline = time.monotonic() + 10
        while 'src.rs' not in screen('files'):
            assert time.monotonic() < deadline
            time.sleep(.05)
        request('keys', keys='<space>')
        deadline = time.monotonic() + 10
        while 'println!' not in screen('diff'):
            assert time.monotonic() < deadline
            time.sleep(.05)
        assert request('state')['mode'] == 'file_diff'
        small = request('dump', width=70, height=20)['screen']
        (out/'diff-small.txt').write_text(small)
        assert 'println!' in small
        request('keys', keys='<esc> <esc>')
        # File filtering uses the background path index and must deliver a
        # real match (only the root commit changed src.rs).
        request('keys', keys='<c-s-f> ' + ' '.join('file=src.rs') + ' <enter>')
        deadline = time.monotonic() + 15
        while True:
            filtered = screen('filtered')
            if 'Commit 00001' in filtered and 'Commit 03000' not in filtered:
                break
            assert time.monotonic() < deadline, filtered
            time.sleep(.05)
        request('keys', keys='?')
        assert 'Help' in screen('help') or 'Keyboard' in (out/'help.txt').read_text()
        request('keys', keys='<esc>')
        request('keys', keys='<c-q>')
        proc.wait(timeout=10)
        thread.join(timeout=2)
        if pixel:
            assert b"\x1b_G" in (out/"terminal.raw").read_bytes(), "Kitty image protocol was not emitted"
        (out/'measurements.json').write_text(json.dumps(dict(startup_to_ready_ms=startup_ms, requests=timings), indent=2))
        print(json.dumps(dict(output=str(out), startup_to_ready_ms=startup_ms, commits=state['commit_count'], verified=True)))
    finally:
        if proc.poll() is None:
            proc.terminate()
            try: proc.wait(timeout=3)
            except subprocess.TimeoutExpired: proc.kill(); proc.wait()
        stopping.set()
        thread.join(timeout=2)
        os.close(master)
