use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub const DAEMON_BIN_NAME: &str = "plamo-translated";

/// Candidates are returned as found, without resolving symlinks, so a `nix profile`
/// path stays valid in the plist after the profile is upgraded.
pub fn locate_daemon(
    explicit: Option<&Path>,
    env_bin: Option<&OsStr>,
    current_exe: &Path,
    path_var: Option<&OsStr>,
) -> Option<PathBuf> {
    let libexec = current_exe
        .parent()
        .and_then(Path::parent)
        .map(|prefix| prefix.join("libexec/bin").join(DAEMON_BIN_NAME));
    let on_path = path_var
        .into_iter()
        .flat_map(std::env::split_paths)
        .map(|dir| dir.join(DAEMON_BIN_NAME));

    explicit
        .map(Path::to_path_buf)
        .into_iter()
        .chain(env_bin.filter(|v| !v.is_empty()).map(PathBuf::from))
        .chain(libexec)
        .chain(on_path)
        .find(|p| is_executable(p))
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executable(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn locate_prefers_explicit_then_env_then_libexec_then_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let explicit = root.join("explicit/plamo-translated");
        let env_bin = root.join("env/plamo-translated");
        let current_exe = root.join("prefix/bin/plamo-translate");
        let libexec = root.join("prefix/libexec/bin/plamo-translated");
        let path_dir = root.join("pathdir");
        let on_path = path_dir.join(DAEMON_BIN_NAME);
        for p in [&explicit, &env_bin, &current_exe, &libexec, &on_path] {
            executable(p);
        }
        let path_var = std::env::join_paths([root.join("empty"), path_dir.clone()]).unwrap();
        let find = || {
            locate_daemon(
                Some(&explicit),
                Some(env_bin.as_os_str()),
                &current_exe,
                Some(&path_var),
            )
        };

        assert_eq!(find(), Some(explicit.clone()));
        std::fs::remove_file(&explicit).unwrap();
        assert_eq!(find(), Some(env_bin.clone()));
        std::fs::remove_file(&env_bin).unwrap();
        assert_eq!(find(), Some(libexec.clone()));
        std::fs::remove_file(&libexec).unwrap();
        assert_eq!(find(), Some(on_path.clone()));
        std::fs::remove_file(&on_path).unwrap();
        assert_eq!(find(), None);
    }

    #[test]
    fn locate_skips_non_executable_files() {
        let tmp = tempfile::tempdir().unwrap();
        let plain = tmp.path().join("plamo-translated");
        std::fs::write(&plain, "").unwrap();
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
        let exe = tmp.path().join("bin/plamo-translate");
        assert_eq!(locate_daemon(Some(&plain), None, &exe, None), None);
    }

    #[test]
    fn locate_does_not_resolve_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("store/plamo-translated");
        executable(&real);
        let profile_bin = tmp.path().join("profile/bin");
        std::fs::create_dir_all(&profile_bin).unwrap();
        let link = profile_bin.join(DAEMON_BIN_NAME);
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let exe = tmp.path().join("elsewhere/bin/plamo-translate");

        assert_eq!(
            locate_daemon(Some(&link), None, &exe, None),
            Some(link.clone())
        );
        let path_var = profile_bin.into_os_string();
        assert_eq!(locate_daemon(None, None, &exe, Some(&path_var)), Some(link));
    }
}
