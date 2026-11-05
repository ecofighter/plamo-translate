import pytest

from plamo_translated.config import (
    DEFAULT_MODEL,
    DEFAULT_REVISION,
    find_config_file,
    load_config,
    paths,
)

APP_SUPPORT = "Library/Application Support/plamo-translate"


def write(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")
    return path


def test_load_config_missing_file_uses_defaults(tmp_path):
    c = load_config({}, tmp_path)
    assert (c.model, c.revision, c.revision_pinned, c.idle_timeout_secs) == (
        DEFAULT_MODEL,
        DEFAULT_REVISION,
        True,
        600,
    )
    assert c.socket_path == tmp_path / APP_SUPPORT / "daemon.sock"
    assert c.config_file is None


def test_default_xdg_location_is_dot_config(tmp_path):
    f = write(tmp_path / ".config/plamo-translate/config.toml", "idle_timeout_secs = 5\n")
    assert find_config_file({}, tmp_path) == f
    assert load_config({}, tmp_path).idle_timeout_secs == 5


def test_env_config_path_wins(tmp_path):
    write(tmp_path / "xdg/plamo-translate/config.toml", "idle_timeout_secs = 1\n")
    explicit = write(tmp_path / "other.toml", "idle_timeout_secs = 2\n")
    env = {"XDG_CONFIG_HOME": str(tmp_path / "xdg"), "PLAMO_TRANSLATE_CONFIG": str(explicit)}
    c = load_config(env, tmp_path)
    assert c.idle_timeout_secs == 2
    assert c.config_file == explicit


def test_xdg_config_home_respected(tmp_path):
    f = write(tmp_path / "xdg/plamo-translate/config.toml", "idle_timeout_secs = 0\n")
    c = load_config({"XDG_CONFIG_HOME": str(tmp_path / "xdg")}, tmp_path)
    assert c.idle_timeout_secs == 0
    assert c.config_file == f


def test_missing_env_config_falls_through_to_xdg(tmp_path):
    f = write(tmp_path / ".config/plamo-translate/config.toml", "")
    env = {"PLAMO_TRANSLATE_CONFIG": str(tmp_path / "nope.toml")}
    assert find_config_file(env, tmp_path) == f


def test_tilde_expanded_in_socket_path(tmp_path):
    write(tmp_path / ".config/plamo-translate/config.toml", 'socket_path = "~/x.sock"\n')
    assert load_config({}, tmp_path).socket_path == tmp_path / "x.sock"


def test_non_default_model_without_revision_uses_main(tmp_path):
    write(
        tmp_path / ".config/plamo-translate/config.toml",
        'model = "pfnet/plamo-3-translate-8b-MLX-4bit"\n',
    )
    c = load_config({}, tmp_path)
    assert c.model == "pfnet/plamo-3-translate-8b-MLX-4bit"
    assert c.revision == "main"
    assert c.revision_pinned is False


def test_non_default_model_with_revision_is_pinned(tmp_path):
    write(
        tmp_path / ".config/plamo-translate/config.toml",
        'model = "x/y"\nrevision = "abc123"\n',
    )
    c = load_config({}, tmp_path)
    assert (c.revision, c.revision_pinned) == ("abc123", True)


def test_default_model_with_explicit_revision_keeps_it(tmp_path):
    write(tmp_path / ".config/plamo-translate/config.toml", 'revision = "deadbeef"\n')
    c = load_config({}, tmp_path)
    assert (c.model, c.revision, c.revision_pinned) == (DEFAULT_MODEL, "deadbeef", True)


def test_env_socket_overrides_config(tmp_path):
    write(tmp_path / ".config/plamo-translate/config.toml", 'socket_path = "/from/config.sock"\n')
    c = load_config({"PLAMO_TRANSLATE_SOCKET": "~/env.sock"}, tmp_path)
    assert c.socket_path == tmp_path / "env.sock"


def test_unused_and_unknown_keys_are_ignored(tmp_path):
    write(
        tmp_path / ".config/plamo-translate/config.toml",
        'default_target = "en"\nglossary_file = "~/g.tsv"\nfuture_key = 1\n',
    )
    assert load_config({}, tmp_path).model == DEFAULT_MODEL


def test_invalid_toml_raises_value_error_with_path(tmp_path):
    f = write(tmp_path / ".config/plamo-translate/config.toml", "model = [unterminated\n")
    with pytest.raises(ValueError, match=str(f)):
        load_config({}, tmp_path)


@pytest.mark.parametrize(
    "line", ["idle_timeout_secs = -1", 'idle_timeout_secs = "10"', "idle_timeout_secs = true", "idle_timeout_secs = 1.5"]
)
def test_invalid_idle_timeout_raises_value_error_with_path(tmp_path, line):
    f = write(tmp_path / ".config/plamo-translate/config.toml", line + "\n")
    with pytest.raises(ValueError, match=str(f)):
        load_config({}, tmp_path)


@pytest.mark.parametrize("line", ["model = 1", 'revision = ""', "socket_path = 3"])
def test_invalid_string_keys_raise_value_error(tmp_path, line):
    f = write(tmp_path / ".config/plamo-translate/config.toml", line + "\n")
    with pytest.raises(ValueError, match=str(f)):
        load_config({}, tmp_path)


def test_paths_layout(tmp_path):
    p = paths(tmp_path)
    base = tmp_path / APP_SUPPORT
    assert p.data_dir == base
    assert p.hub_cache == base / "huggingface/hub"
    assert p.modules_cache == base / "huggingface/modules"
    assert p.log_dir == tmp_path / "Library/Logs/plamo-translate"
    assert p.default_socket == base / "daemon.sock"
