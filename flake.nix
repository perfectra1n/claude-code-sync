{
  description = "Sync Claude Code conversation history with git repositories";

  # Usage:
  #   nix run github:perfectra1n/claude-code-sync -- --help
  #   nix profile install github:perfectra1n/claude-code-sync
  # or add this flake as an input and use `packages.<system>.default`.

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
    in
    {
      packages = forAllSystems (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = cargoToml.package.name;
          inherit (cargoToml.package) version;

          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./src
            ];
          };
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = [ pkgs.makeWrapper ];

          # The suite needs a git identity, Mercurial and --test-threads=1
          # (see mise.toml); CI runs it, the package build does not.
          doCheck = false;

          # The tool shells out to git. Suffix rather than prefix PATH so the
          # user's own git, with their config and credential helpers, wins.
          postInstall = ''
            wrapProgram $out/bin/claude-code-sync --suffix PATH : ${pkgs.lib.makeBinPath [ pkgs.git ]}
          '';

          meta = {
            inherit (cargoToml.package) description;
            homepage = cargoToml.package.repository;
            license = pkgs.lib.licenses.mit;
            mainProgram = "claude-code-sync";
          };
        };
      });

      apps = forAllSystems (pkgs: {
        default = {
          type = "app";
          program = pkgs.lib.getExe self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        };
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${pkgs.stdenv.hostPlatform.system}.default ];
          packages = [
            pkgs.mise
            pkgs.mercurial
          ];
        };
      });
    };
}
