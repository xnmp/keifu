{
  description = "keifu: a TUI to visualize Git commit graphs with branch genealogy";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        lib = pkgs.lib;
      in
      {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "keifu";
          version = (lib.importTOML ./Cargo.toml).package.version;

          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;

          # Deliberately no native buildInputs: the crate tree avoids openssl and
          # chafa by policy (see Cargo.toml comments); libgit2 builds via the
          # bundled cc path.

          meta = {
            description = "A TUI tool to visualize Git commit graphs with branch genealogy";
            homepage = "https://github.com/xnmp/keifu";
            license = lib.licenses.mit;
            mainProgram = "keifu";
          };
        };

        apps.default = flake-utils.lib.mkApp {
          drv = self.packages.${system}.default;
          name = "keifu";
        };
      }
    );
}
