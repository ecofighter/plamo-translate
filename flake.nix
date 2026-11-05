{
  description = "Local Japanese-English translation with PLaMo on Apple silicon";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    home-manager = {
      url = "github:nix-community/home-manager";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
      home-manager,
      ...
    }:
    let
      system = "aarch64-darwin";
      overlay = import ./nix/overlay.nix;
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ overlay ];
      };
      craneLib = crane.mkLib pkgs;
      build = import ./nix/packages.nix { inherit pkgs craneLib; };
      inherit (build) packages;

      daemonApp =
        cmd:
        let
          script = pkgs.writeShellApplication {
            name = "daemon-${cmd}";
            text = ''exec ${packages.default}/bin/plamo-translate daemon ${cmd} "$@"'';
          };
        in
        {
          type = "app";
          program = "${script}/bin/daemon-${cmd}";
          meta.description = "Run plamo-translate daemon ${cmd}";
        };
    in
    {
      overlays.default = overlay;

      homeManagerModules.default = import ./nix/hm-module.nix { inherit self; };

      packages.${system} = packages;

      apps.${system} = nixpkgs.lib.genAttrs' [
        "install"
        "uninstall"
        "start"
        "stop"
        "restart"
        "status"
        "logs"
      ] (cmd: nixpkgs.lib.nameValuePair "daemon-${cmd}" (daemonApp cmd));

      checks.${system} = import ./nix/checks.nix {
        inherit pkgs craneLib packages;
        inherit home-manager;
        hmModule = self.homeManagerModules.default;
        inherit (build) cargoArgs cargoArtifacts;
      };

      devShells.${system}.default = pkgs.mkShell {
        packages = [
          (import ./nix/python-env.nix {
            inherit (pkgs) python3;
            withTests = true;
          })
          pkgs.cargo
          pkgs.rustc
          pkgs.rustfmt
          pkgs.clippy
          pkgs.rust-analyzer
          pkgs.just
          pkgs.cargo-insta
        ];
      };
    };
}
