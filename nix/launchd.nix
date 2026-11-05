{ lib }:

{
  homeDirectory,
  package,
  settings,
}:

let
  logDir = "${homeDirectory}/Library/Logs/plamo-translate";
  defaultSocket = "${homeDirectory}/Library/Application Support/plamo-translate/daemon.sock";
in
# Keys and values mirror the CLI's `daemon install` plist (daemon/plist.rs); only
# ProgramArguments differs once home-manager wraps it with wait4path.
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
  EnvironmentVariables.PLAMO_TRANSLATE_MANAGED_BY = "nix";
}
