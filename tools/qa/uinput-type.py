#!/usr/bin/env python3
"""Types a short key sequence through a temporary /dev/uinput keyboard (QA IME test).

Usage: uinput-type.py "nihao" "<space>" ...
Each argument is a run of lowercase letters/digits or one of <space> <enter> <esc> <bs>.
The device exists only while this script runs. Requires write access to /dev/uinput.
"""
import fcntl
import os
import struct
import sys
import time

UI_SET_EVBIT = 0x40045564
UI_SET_KEYBIT = 0x40045565
UI_DEV_SETUP = 0x405C5503
UI_DEV_CREATE = 0x5501
UI_DEV_DESTROY = 0x5502
EV_SYN, EV_KEY, SYN_REPORT = 0, 1, 0

LETTERS = "qwertyuiop", "asdfghjkl", "zxcvbnm"
KEYS = {}
for row, start in zip(LETTERS, (16, 30, 44)):
    for i, ch in enumerate(row):
        KEYS[ch] = start + i
for i, ch in enumerate("1234567890"):
    KEYS[ch] = 2 + i
NAMED = {"<space>": 57, "<enter>": 28, "<esc>": 1, "<bs>": 14}


def emit(fd, etype, code, value):
    os.write(fd, struct.pack("llHHi", 0, 0, etype, code, value))


def tap(fd, code):
    emit(fd, EV_KEY, code, 1)
    emit(fd, EV_SYN, SYN_REPORT, 0)
    time.sleep(0.03)
    emit(fd, EV_KEY, code, 0)
    emit(fd, EV_SYN, SYN_REPORT, 0)
    time.sleep(0.06)


def main(args):
    seq = []
    for a in args:
        if a in NAMED:
            seq.append(NAMED[a])
        else:
            seq.extend(KEYS[c] for c in a)
    fd = os.open("/dev/uinput", os.O_WRONLY | os.O_NONBLOCK)
    try:
        fcntl.ioctl(fd, UI_SET_EVBIT, EV_KEY)
        for code in set(list(KEYS.values()) + list(NAMED.values())):
            fcntl.ioctl(fd, UI_SET_KEYBIT, code)
        setup = struct.pack("HHHH80sI", 0x06, 0x1234, 0x5678, 1, b"ytm-glass-qa-keyboard", 0)
        fcntl.ioctl(fd, UI_DEV_SETUP, setup)
        fcntl.ioctl(fd, UI_DEV_CREATE)
        time.sleep(1.0)  # let libinput/KWin pick up the device
        for code in seq:
            tap(fd, code)
        time.sleep(0.2)
        fcntl.ioctl(fd, UI_DEV_DESTROY)
    finally:
        os.close(fd)
    print(f"typed {len(seq)} keys")


if __name__ == "__main__":
    main(sys.argv[1:])
