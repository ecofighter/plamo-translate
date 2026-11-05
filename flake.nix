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
    { nixpkgs, ... }:
    let
      system = "aarch64-darwin";
      overlay = import ./nix/overlay.nix;
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ overlay ];
      };
    in
    {
      overlays.default = overlay;

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
