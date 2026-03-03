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
        pkgs = import nixpkgs { inherit system overlays; };
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" ];
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
          ];

          shellHook = ''
            echo "wreck-it development environment"
            echo "Rust version: $(rustc --version)"
          '';
        };

        packages = {
          default = wreck-it;
          wreck-it = wreck-it;
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
