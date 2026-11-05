from __future__ import annotations

import argparse
import asyncio
import importlib
import logging
import os
import signal
import subprocess
import sys
import threading
from logging.handlers import RotatingFileHandler
from pathlib import Path
from typing import NoReturn

from . import __version__
from .activation import bind_socket, launchd_socket
from .config import Config, Paths, load_config, paths
from .engine import Engine
from .server import Server

LOG_MAX_BYTES = 5 * 1024 * 1024
LOG_BACKUPS = 3
LOG_FORMAT = "%(asctime)s %(levelname)s %(name)s: %(message)s"

log = logging.getLogger("plamo_translated")


def make_engine(config: Config, paths: Paths) -> Engine:
    from .mlx_engine import MlxEngine  # imports mlx; keep it out of module import

    return MlxEngine(config.model, config.revision, paths.hub_cache)


def exclude_from_time_machine(path: Path) -> None:
    try:
        subprocess.run(["tmutil", "addexclusion", str(path)], check=True, capture_output=True)
    except (OSError, subprocess.SubprocessError) as e:
        log.warning("tmutil addexclusion failed for %s: %s", path, e)


def _prepare_directories(p: Paths) -> bool:
    """Returns True when <data>/huggingface was created by this call."""
    p.data_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
    p.log_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
    hf_dir = p.hub_cache.parent
    created = not hf_dir.exists()
    p.hub_cache.mkdir(parents=True, exist_ok=True)
    p.modules_cache.mkdir(parents=True, exist_ok=True)
    return created


def _setup_logging(log_dir: Path) -> RotatingFileHandler:
    handler = RotatingFileHandler(
        log_dir / "daemon.log", maxBytes=LOG_MAX_BYTES, backupCount=LOG_BACKUPS, encoding="utf-8"
    )
    handler.setFormatter(logging.Formatter(LOG_FORMAT))
    root = logging.getLogger()
    root.addHandler(handler)
    root.setLevel(logging.INFO)
    # Route library warnings into daemon.log instead of an unbounded stderr.
    logging.captureWarnings(True)
    return handler


def route_library_logging() -> None:
    """transformers and huggingface_hub attach their own stderr handlers; send their records to the root logger."""
    for name, module in (
        ("transformers", "transformers.utils.logging"),
        ("huggingface_hub", "huggingface_hub.utils.logging"),
    ):
        try:
            importlib.import_module(module).enable_propagation()
        except ImportError:
            continue
        library_logger = logging.getLogger(name)
        for handler in list(library_logger.handlers):
            if type(handler) is logging.StreamHandler:
                library_logger.removeHandler(handler)


def _teardown_logging(handler: RotatingFileHandler) -> None:
    logging.captureWarnings(False)
    logging.getLogger().removeHandler(handler)
    handler.flush()
    handler.close()


async def _serve(server: Server, sock) -> None:
    # Signal handlers can only be installed from the main thread (tests run main() in a worker thread).
    if threading.current_thread() is threading.main_thread():
        asyncio.get_running_loop().add_signal_handler(signal.SIGTERM, server.request_shutdown)
    await server.serve(sock)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="plamo-translated")
    parser.add_argument("--socket", type=Path, help="bind this path instead of using launchd socket activation")
    parser.add_argument("--version", action="store_true")
    args = parser.parse_args(argv)
    if args.version:
        print(f"plamo-translated {__version__}")
        return 0

    config = load_config(os.environ, Path.home())
    p = paths(Path.home())
    os.environ["HF_HUB_CACHE"] = str(p.hub_cache)
    os.environ["HF_MODULES_CACHE"] = str(p.modules_cache)
    os.environ.setdefault("MLX_ENABLE_TF32", "0")
    os.environ.setdefault("MLX_ENABLE_CACHE_THRASHING_CHECK", "0")

    created_hf = _prepare_directories(p)
    handler = _setup_logging(p.log_dir)
    try:
        if created_hf:
            exclude_from_time_machine(p.hub_cache.parent)
        log.info("starting plamo-translated %s (model %s @ %s)", __version__, config.model, config.revision)
        if not config.revision_pinned:
            log.warning("revision is not pinned; tracking %r", config.revision)
        sock = bind_socket(args.socket) if args.socket else launchd_socket()
        engine = make_engine(config, p)
        route_library_logging()
        server = Server(engine, config.idle_timeout_secs, __version__)
        asyncio.run(_serve(server, sock))
        log.info("exiting")
        return 0
    except Exception:
        log.exception("fatal error")
        raise
    finally:
        _teardown_logging(handler)


def run() -> NoReturn:
    # A worker thread stuck in a model load must not keep the process alive after serve() returns.
    try:
        code = main()
    except SystemExit as e:
        code = e.code if isinstance(e.code, int) else 1
    except BaseException:
        sys.excepthook(*sys.exc_info())
        code = 1
    sys.stdout.flush()
    sys.stderr.flush()
    os._exit(code)


if __name__ == "__main__":
    run()
