{ lib }:

{
  homeDirectory,
  package,
  settings,
  xdgConfigHome,
}:

let
  logDir = "${homeDirectory}/Library/Logs/plamo-translate";
  defaultSocket = "${homeDirectory}/Library/Application Support/plamo-translate/daemon.sock";
in
# Keys and values mirror the CLI's `daemon install` plist (daemon/plist.rs); only
# ProgramArguments differs once home-manager wraps it with wait4path, and only the
# Nix plist sets XDG_CONFIG_HOME (home-manager knows xdg.configHome; the CLI does not need to).
{
  Label = "local.plamo-translate.daemon";
  ProgramArguments = [ "${package}/libexec/bin/plamo-translated" ];
  Sockets.Listeners = {
    SockPathName = settings.socket_path or defaultSocket;
    SockPathMode = 384;
  };
  StandardOutPath = "${logDir}/daemon.stderr.log";
  StandardErrorPath = "${logDir}/daemon.stderr.log";
  ProcessType = "Interactive";
  EnvironmentVariables = {
    PLAMO_TRANSLATE_MANAGED_BY = "nix";
    # launchd does not pass the login shell's XDG_CONFIG_HOME, so the daemon would
    # otherwise miss a config.toml placed under a custom xdg.configHome.
    XDG_CONFIG_HOME = xdgConfigHome;
  };
}
