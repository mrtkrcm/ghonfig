#!/usr/bin/env python3
"""End-to-end TUI tests: drive the real binary in a pty, assert on the file.

These cover what `cargo test` cannot — that a keypress reaches the config on
disk, that an invalid value never does, and that the file comes back with its
comments and mode intact. Every destructive path runs against a throwaway
config via GHOSTTY_CONFIG, never the real one.

    pip install pyte && python3 tests/tui_e2e.py
"""
import os, pty, sys, time, select, fcntl, termios, struct, signal, shutil, subprocess

BIN = os.path.expanduser("~/.cargo/bin/ghonfig")
DIR = "/tmp/ghonfig-tui-test"
CFG = os.path.join(DIR, "config")
SEED = """# a comment that must survive
theme = Nord
font-size = 12
window-padding-x = 4
"""

def drive(keys, settle=2.5, delay=1.0, cols=110, rows=30):
    pid, fd = pty.fork()
    if pid == 0:
        os.environ["TERM"] = "xterm-256color"
        os.environ["GHOSTTY_CONFIG"] = CFG
        os.execv(BIN, ["ghonfig"])
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
    buf = b""
    def pump(t):
        nonlocal buf
        end = time.time() + t
        while time.time() < end:
            r, _, _ = select.select([fd], [], [], 0.1)
            if r:
                try: buf += os.read(fd, 65536)
                except OSError: return
    pump(settle)
    for k in keys:
        os.write(fd, k.encode())
        time.sleep(delay)
        pump(0.6)
    pump(0.8)
    try: os.kill(pid, signal.SIGKILL)
    except ProcessLookupError: pass
    os.close(fd)
    try: os.waitpid(pid, 0)
    except ChildProcessError: pass
    import pyte
    screen = pyte.Screen(cols, rows); pyte.Stream(screen).feed(buf.decode("utf8", "replace"))
    return "\n".join(l.rstrip() for l in screen.display)

def reset():
    shutil.rmtree(DIR, ignore_errors=True)
    os.makedirs(DIR)
    open(CFG, "w").write(SEED)
    os.chmod(CFG, 0o600)

results = []
def check(name, cond, extra=""):
    results.append((name, cond))
    print(("  PASS  " if cond else "  FAIL  ") + name + (("\n         " + extra) if (extra and not cond) else ""))

# ── 1. edit a value and SAVE ────────────────────────────────────────────────
reset()
scr = drive(list("/font-size") + ["\r", "\r", "\x15"] + list("18") + ["\r", "s"])
body = open(CFG).read()
# ^U clears, so this asserts a replaced value — not the append that would
# result from typing over a value the cursor is sitting at the end of.
check("save writes the edited value", "font-size = 18" in body, body)
check("comment survived the save", "# a comment that must survive" in body, body)
check("untouched keys survived", "theme = Nord" in body and "window-padding-x = 4" in body, body)
check("mode still 0600", oct(os.stat(CFG).st_mode & 0o777) == "0o600", oct(os.stat(CFG).st_mode & 0o777))
check("backup written", os.path.exists(CFG + ".bak"))
check("toast confirms save", "saved" in scr, scr.splitlines()[-3:])

# ── 2. unset removes the key ────────────────────────────────────────────────
reset()
drive(list("/window-padding-x") + ["\r", "\r"[:0]] + ["d", "s"])
body = open(CFG).read()
check("unset removed the key", "window-padding-x" not in body, body)
check("unset kept the others", "theme = Nord" in body and "font-size = 12" in body, body)

# ── 3. quit guard on unsaved edits ──────────────────────────────────────────
reset()
scr = drive(list("/font-size") + ["\r", "\r"] + list("20") + ["\r", "q"])
check("q with unsaved edits does NOT exit", "ghonfig" in scr and "Set" in scr, scr.splitlines()[:2])
check("q warns about unsaved changes", "unsaved changes" in scr, scr.splitlines()[-3:])
check("nothing written to disk", "font-size = 12" in open(CFG).read(), open(CFG).read())

# ── 4. invalid value is refused at save ─────────────────────────────────────
reset()
scr = drive(list("/font-size") + ["\r", "\r"] + list("banana") + ["\r", "s"])
check("invalid value refused, file untouched", "font-size = 12" in open(CFG).read(), open(CFG).read())
check("error surfaced in the UI", "invalid value" in scr, scr.splitlines()[-3:])

# ── 5. setting a key that is NOT yet in the file (All tab, append path) ─────
reset()
scr = drive(["\t"] + list("/cursor-style") + ["\r", "\r"] + ["\t", "\r", "s"])
body = open(CFG).read()
check("new key appended from the All tab", "cursor-style =" in body, body)
check("append did not disturb existing lines", "theme = Nord" in body and "font-size = 12" in body, body)

# ── 5b. typing without ^U appends at the cursor (documented behaviour) ──────
reset()
drive(list("/font-size") + ["\r", "\r"] + list("8") + ["\r", "s"])
check("no ^U: typing appends at the cursor", "font-size = 128" in open(CFG).read(), open(CFG).read())

# ── 6. narrow terminal does not panic ───────────────────────────────────────
reset()
scr = drive(["j", "j"], cols=52, rows=14)
check("survives a 52x14 terminal", "ghonfig" in scr, scr)

print()
bad = [n for n, ok in results if not ok]
print(f"{len(results)-len(bad)} passed, {len(bad)} failed")
sys.exit(1 if bad else 0)
