{
  description = "Invokr - distributed job scheduling and execution engine";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" ];
          targets = [ "wasm32-unknown-unknown" ];
        };
      in {
        packages.smithy-cli = pkgs.callPackage ./nix/smithy-cli.nix { };

        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            rustToolchain
            pkg-config
            openssl
            postgresql
            git
            docker-compose
            sqlx-cli
            nodejs_22
            yarn
            self.packages.${system}.smithy-cli
            just
            wasm-pack
            wasm-bindgen-cli
            tailwindcss
            awscli2
          ];

          shellHook = ''
            echo "Invokr dev shell ready"
            export DATABASE_URL="postgresql://invokr:invokr@localhost:5434/invokr_db"
          '';
        };
      });
}
