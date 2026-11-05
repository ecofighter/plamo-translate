from __future__ import annotations

import contextlib
import ctypes
import socket
from pathlib import Path

LISTEN_BACKLOG = 64


def launchd_socket(name: str = "Listeners") -> socket.socket:
    libc = ctypes.CDLL(None)
    libc.launch_activate_socket.argtypes = [
        ctypes.c_char_p,
        ctypes.POINTER(ctypes.POINTER(ctypes.c_int)),
        ctypes.POINTER(ctypes.c_size_t),
    ]
    libc.launch_activate_socket.restype = ctypes.c_int
    libc.free.argtypes = [ctypes.c_void_p]
    fds = ctypes.POINTER(ctypes.c_int)()
    count = ctypes.c_size_t()
    rc = libc.launch_activate_socket(name.encode(), ctypes.byref(fds), ctypes.byref(count))
    if rc != 0:
        raise RuntimeError(f"launch_activate_socket({name!r}) failed with error {rc}; not started by launchd?")
    try:
        received = [fds[i] for i in range(count.value)]
    finally:
        libc.free(fds)
    if len(received) != 1:
        raise RuntimeError(f"expected exactly one launchd socket for {name!r}, got {len(received)}")
    return socket.socket(fileno=received[0])


def bind_socket(path: Path) -> socket.socket:
    with contextlib.suppress(FileNotFoundError):
        path.unlink()
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        sock.bind(str(path))
        path.chmod(0o600)
        sock.listen(LISTEN_BACKLOG)
    except BaseException:
        sock.close()
        raise
    return sock
