{
  description = "wreck-it - A TUI agent harness for Ralph Wiggum loops";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
          config.allowUnfree = true;
        };
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" ];
        };

        # GitHub Copilot CLI — pre-built binary package
        copilotVersion = "latest";
        copilotMeta = {
          "x86_64-linux" = {
            platform = "linux";
            arch = "x64";
            sha256 =
              "344f89ee1e619c9ffcb072381b7e53c2f305053008b55584dc5846d003c167b9";
          };
          "aarch64-linux" = {
            platform = "linux";
            arch = "arm64";
            sha256 =
              "60afebcf47644db1cedd4bbe2be9fc18e6ffb4bc394f570a7896b7a8fd889f63";
          };
          "x86_64-darwin" = {
            platform = "darwin";
            arch = "x64";
            sha256 =
              "cd47e1d5287f724be05acfd8564a4628bf6f622dde8156f8b269e1641409989b";
          };
          "aarch64-darwin" = {
            platform = "darwin";
            arch = "arm64";
            sha256 =
              "8216312d266329ea0b1440cc2e685c66b6a26977cfa3d02b0388d6051f3fb6ab";
          };
        };
        copilotInfo = copilotMeta.${system} or (throw
          "copilot-cli: unsupported system ${system}");
        copilot-cli = pkgs.stdenv.mkDerivation {
          pname = "copilot-cli";
          version = copilotVersion;
          src = pkgs.fetchurl {
            url =
              "https://github.com/github/copilot-cli/releases/latest/download/copilot-${copilotInfo.platform}-${copilotInfo.arch}.tar.gz";
            sha256 = copilotInfo.sha256;
          };
          sourceRoot = ".";
          unpackPhase = ''
            mkdir -p src
            tar -xzf $src -C src
          '';
          installPhase = ''
            mkdir -p $out/bin
            cp src/copilot $out/bin/copilot
            chmod +x $out/bin/copilot
          '';
          meta = with pkgs.lib; {
            description = "GitHub Copilot CLI";
            homepage = "https://github.com/github/copilot-cli";
            license = licenses.unfree;
            platforms = [
              "x86_64-linux"
              "aarch64-linux"
              "x86_64-darwin"
              "aarch64-darwin"
            ];
          };
        };

        wreck-it = pkgs.rustPlatform.buildRustPackage {
          pname = "wreck-it";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          nativeCheckInputs = with pkgs; [ git ];
        };
        wreck-it-dev = pkgs.writeShellScriptBin "wreck-it" ''
          exec cargo run -- "$@"
        '';
      in {
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            rustToolchain
            cargo
            rustc
            rust-analyzer
            pkg-config
            openssl
            wreck-it-dev
            copilot-cli
          ];

          shellHook = ''
            echo "wreck-it development environment"
            echo "Rust version: $(rustc --version)"
          '';
        };

        packages = {
          default = wreck-it;
          wreck-it = wreck-it;
          copilot-cli = copilot-cli;
        };

        apps.build-app = {
          type = "app";
          program = toString (pkgs.writeShellScript "build-app" ''
            set -euo pipefail
            echo "▸ Building Rust static library…"
            cargo build --release -p wreck-it
            echo "▸ Building WreckItBoard.app…"
            xcodebuild -project WreckItBoard/WreckItBoard.xcodeproj \
              -scheme WreckItBoard \
              -configuration Release \
              build
          '');
        };
      });
}
