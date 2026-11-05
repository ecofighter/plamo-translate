from __future__ import annotations

import tomllib
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path

DEFAULT_MODEL = "pfnet/plamo-3-translate-2b-MLX-4bit"
DEFAULT_REVISION = "63777ddc6dc80aacacd92a1bf6966105f2f6270c"
DEFAULT_IDLE_TIMEOUT_SECS = 600
UNPINNED_REVISION = "main"


@dataclass(frozen=True)
class Paths:
    data_dir: Path
    hub_cache: Path
    modules_cache: Path
    log_dir: Path
    default_socket: Path


@dataclass(frozen=True)
class Config:
    model: str
    revision: str
    revision_pinned: bool
    idle_timeout_secs: int
    socket_path: Path
    config_file: Path | None


def paths(home: Path) -> Paths:
    data_dir = home / "Library/Application Support/plamo-translate"
    return Paths(
        data_dir=data_dir,
        hub_cache=data_dir / "huggingface/hub",
        modules_cache=data_dir / "huggingface/modules",
        log_dir=home / "Library/Logs/plamo-translate",
        default_socket=data_dir / "daemon.sock",
    )


def _expand(value: str, home: Path) -> Path:
    if value == "~":
        return home
    if value.startswith("~/"):
        return home / value[2:]
    return Path(value)


def find_config_file(env: Mapping[str, str], home: Path) -> Path | None:
    candidates: list[Path] = []
    if env.get("PLAMO_TRANSLATE_CONFIG"):
        candidates.append(_expand(env["PLAMO_TRANSLATE_CONFIG"], home))
    xdg = env.get("XDG_CONFIG_HOME")
    config_home = _expand(xdg, home) if xdg else home / ".config"
    candidates.append(config_home / "plamo-translate/config.toml")
    return next((c for c in candidates if c.is_file()), None)


def _read_toml(path: Path) -> dict:
    try:
        with path.open("rb") as f:
            return tomllib.load(f)
    except (tomllib.TOMLDecodeError, UnicodeDecodeError) as e:
        raise ValueError(f"{path}: invalid TOML: {e}") from e


def _optional_str(raw: dict, key: str, path: Path) -> str | None:
    value = raw.get(key)
    if value is None:
        return None
    if not isinstance(value, str) or not value:
        raise ValueError(f"{path}: {key} must be a non-empty string")
    return value


def load_config(env: Mapping[str, str], home: Path) -> Config:
    config_file = find_config_file(env, home)
    raw = _read_toml(config_file) if config_file else {}
    where = config_file or Path("<defaults>")

    model = _optional_str(raw, "model", where) or DEFAULT_MODEL
    revision = _optional_str(raw, "revision", where)
    if revision is None and model == DEFAULT_MODEL:
        revision = DEFAULT_REVISION
    revision_pinned = revision is not None

    idle = raw.get("idle_timeout_secs", DEFAULT_IDLE_TIMEOUT_SECS)
    if isinstance(idle, bool) or not isinstance(idle, int) or idle < 0:
        raise ValueError(f"{where}: idle_timeout_secs must be a non-negative integer")

    env_socket = env.get("PLAMO_TRANSLATE_SOCKET")
    config_socket = _optional_str(raw, "socket_path", where)
    socket_value = env_socket or config_socket
    socket_path = _expand(socket_value, home) if socket_value else paths(home).default_socket

    return Config(
        model=model,
        revision=revision or UNPINNED_REVISION,
        revision_pinned=revision_pinned,
        idle_timeout_secs=idle,
        socket_path=socket_path,
        config_file=config_file,
    )
