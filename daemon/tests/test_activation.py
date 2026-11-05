import json
import logging
import os
import shutil
import socket
import stat
import tempfile
import threading
import time
from logging.handlers import RotatingFileHandler
from pathlib import Path

import pytest

import transformers.utils.logging as tl  # noqa: E402  (import notice goes to stderr once)
from huggingface_hub.utils import logging as hl

from conftest import FakeEngine
from plamo_translated import __main__ as entry
from plamo_translated.activation import bind_socket, launchd_socket


@pytest.fixture
def short_dir():
    d = Path(tempfile.mkdtemp(prefix="pt", dir="/tmp"))
    yield d
    shutil.rmtree(d, ignore_errors=True)


def test_bind_socket_sets_mode_0600(short_dir):
    path = short_dir / "d.sock"
    s = bind_socket(path)
    try:
        assert stat.S_IMODE(os.stat(path).st_mode) == 0o600
    finally:
        s.close()


def test_bind_socket_replaces_stale_file(short_dir):
    path = short_dir / "d.sock"
    path.write_text("stale")
    s = bind_socket(path)
    try:
        assert stat.S_ISSOCK(os.stat(path).st_mode)
    finally:
        s.close()


def test_launchd_socket_outside_launchd_raises():
    with pytest.raises(RuntimeError):
        launchd_socket()


def test_main_version(capsys):
    assert entry.main(["--version"]) == 0
    assert "0.1.0" in capsys.readouterr().out


def test_main_with_socket_serves_status_and_exits_on_idle(tmp_path, short_dir, monkeypatch):
    home = tmp_path / "home"
    cfg = home / ".config/plamo-translate/config.toml"
    cfg.parent.mkdir(parents=True)
    cfg.write_text("idle_timeout_secs = 1\n")
    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: home))
    for var in ("PLAMO_TRANSLATE_CONFIG", "PLAMO_TRANSLATE_SOCKET", "XDG_CONFIG_HOME"):
        monkeypatch.delenv(var, raising=False)
    monkeypatch.setenv("HF_HUB_CACHE", "x")
    monkeypatch.setenv("HF_MODULES_CACHE", "x")
    monkeypatch.setattr(entry, "make_engine", lambda config, paths: FakeEngine())
    excluded: list[Path] = []
    monkeypatch.setattr(entry, "exclude_from_time_machine", excluded.append)

    sock = short_dir / "d.sock"
    result: list[int] = []
    t = threading.Thread(target=lambda: result.append(entry.main(["--socket", str(sock)])))
    t.start()
    try:
        deadline = time.monotonic() + 5
        while not sock.exists() and time.monotonic() < deadline:
            time.sleep(0.02)
        with socket.socket(socket.AF_UNIX) as c:
            c.settimeout(5)
            c.connect(str(sock))
            c.sendall(b'{"v":1,"op":"status"}\n')
            reply = json.loads(c.makefile().readline())
        assert reply["type"] == "status"
        assert reply["protocol"] == 1
    finally:
        t.join(10)
    assert result == [0]
    assert (home / "Library/Logs/plamo-translate/daemon.log").exists()
    assert stat.S_IMODE(os.stat(home / "Library/Application Support/plamo-translate").st_mode) == 0o700
    assert excluded == [home / "Library/Application Support/plamo-translate/huggingface"]
    assert os.environ["HF_HUB_CACHE"] == str(home / "Library/Application Support/plamo-translate/huggingface/hub")
    assert not any(isinstance(h, RotatingFileHandler) for h in logging.getLogger().handlers)


def test_exclude_from_time_machine_failure_only_warns(monkeypatch, caplog, tmp_path):
    def boom(*a, **k):
        raise OSError("no tmutil")

    monkeypatch.setattr(entry.subprocess, "run", boom)
    with caplog.at_level(logging.WARNING):
        entry.exclude_from_time_machine(tmp_path)
    assert "tmutil" in caplog.text


def test_route_library_logging_sends_records_to_root_not_stderr(capfd):
    records: list[logging.LogRecord] = []

    class Collect(logging.Handler):
        def emit(self, record):
            records.append(record)

    collector = Collect()
    root = logging.getLogger()
    root.addHandler(collector)
    try:
        entry.route_library_logging()
        tl.get_logger("transformers.test").warning("from-transformers")
        hl.get_logger("huggingface_hub.test").warning("from-hub")
    finally:
        root.removeHandler(collector)
    messages = [r.getMessage() for r in records]
    assert "from-transformers" in messages and "from-hub" in messages
    assert capfd.readouterr().err == ""
