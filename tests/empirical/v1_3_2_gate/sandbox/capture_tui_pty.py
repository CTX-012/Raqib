#!/usr/bin/env python3
import argparse, errno, fcntl, os, pty, signal, struct, sys, termios, time
ap = argparse.ArgumentParser()
ap.add_argument("--bin", required=True)
ap.add_argument("--config", required=True)
ap.add_argument("--port", type=int, default=7070)
ap.add_argument("--cols", type=int, default=140)
ap.add_argument("--rows", type=int, default=40)
ap.add_argument("--settle-secs", type=float, default=6.0)
ap.add_argument("--out", required=True)
args = ap.parse_args()
pid, fd = pty.fork()
if pid == 0:
    os.execv(args.bin, [args.bin, "--bind", "127.0.0.1",
                        "--port", str(args.port), "--config", args.config])
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", args.rows, args.cols, 0, 0))
buf = b""
deadline = time.monotonic() + args.settle_secs
while time.monotonic() < deadline:
    try:
        data = os.read(fd, 4096)
        if not data: break
        buf += data
    except OSError as e:
        if e.errno == errno.EIO: break
        raise
os.write(fd, b"q"); time.sleep(0.5)
try: os.kill(pid, signal.SIGTERM)
except ProcessLookupError: pass
try: os.waitpid(pid, 0)
except ChildProcessError: pass
with open(args.out, "wb") as f: f.write(buf)
print(f"captured {len(buf)} bytes to {args.out}", file=sys.stderr)
