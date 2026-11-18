{
  description = "XERJ: the search engine for AI agents";

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
      workspace = (builtins.fromTOML (builtins.readFile ./engine/Cargo.toml)).workspace.package;
    in
    {
      packages = forAllSystems (pkgs: {
        default = self.packages.${pkgs.stdenv.hostPlatform.system}.xerj;
        xerj = pkgs.rustPlatform.buildRustPackage {
          pname = "xerj";
          inherit (workspace) version;

          # The engine, and the Console assets that xerj-console-api's build
          # script embeds from ../../../xerj-ux. Nothing else, so an edit to
          # the docs or the site does not rebuild the binary.
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              ./engine
              ./xerj-ux
            ];
          };
          cargoRoot = "engine";
          buildAndTestSubdir = "engine";
          cargoLock.lockFile = ./engine/Cargo.lock;

          cargoBuildFlags = [
            "-p"
            "xerj-server"
            "--bin"
            "xerj"
          ];
          # The vendored OpenSSL (openssl-src) is built with perl and make.
          nativeBuildInputs = [ pkgs.perl ];
          # The workspace suite binds ports, starts nodes and runs for a long
          # time; CI runs it. The package check is the binary answering below.
          doCheck = false;
          doInstallCheck = true;
          installCheckPhase = ''
            runHook preInstallCheck
            $out/bin/xerj --version
            runHook postInstallCheck
          '';

          meta = {
            description = "Search engine for AI agents: one local binary that indexes code, docs and logs and serves an Elasticsearch-compatible API and an MCP server";
            homepage = "https://xerj.org";
            license = pkgs.lib.licenses.asl20;
            mainProgram = "xerj";
            platforms = systems;
          };
        };
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = [
            pkgs.cargo
            pkgs.rustc
            pkgs.rustfmt
            pkgs.clippy
            pkgs.rust-analyzer
            pkgs.perl
          ];
          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
        };
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt-rfc-style);
    };
}
