{
  description = "margin — live boards beside the chat, on our own terminal toolkit (vellum)";

  # Pinned in flake.lock. rust-overlay supplies a stable toolchain with the
  # wasm32-wasip1 target, so a later Zellij plugin crate builds from the same
  # shell (nixpkgs' rustc has no wasm target).
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; overlays = [ rust-overlay.overlays.default ]; };
        rust = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" ];
          targets = [ "wasm32-wasip1" ];
        };
      in {
        # devShells.default — `use flake` in .envrc. Runtime tools margin
        # shells out to sit here too: difftastic renders diff blocks, jq for
        # the skill's examples, python+pyte renders a captured client stream
        # to text (we can't see the screen); graphviz + librsvg build the
        # diagrams in docs/ (docs/tools/*.dot). Pillow and JetBrainsMono Nerd
        # Font draw the mockups cell by cell (docs/tools/termshot.py), so Nerd
        # Font icons show exactly as Ghostty would draw them; MARGIN_FONT_DIR
        # tells termshot where the font files live.
        devShells.default = pkgs.mkShell {
          packages = [
            rust
            pkgs.difftastic
            pkgs.jq
            (pkgs.python3.withPackages (ps: [ ps.pyte ps.pillow ]))
            pkgs.graphviz
            pkgs.librsvg
          ];
          MARGIN_FONT_DIR = "${pkgs.nerd-fonts.jetbrains-mono}/share/fonts/truetype/NerdFonts/JetBrainsMono";
        };

        # packages.default: the margin binary. `margin diff` shells out to difftastic,
        # so the wrapper puts difft on its PATH; everything else is in the binary.
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "margin";
          version = "0.1.0";
          src = pkgs.lib.cleanSourceWith {
            src = ./.;
            filter = path: type: !(pkgs.lib.hasInfix "/docs/img" path) && !(pkgs.lib.hasInfix "/target" path);
          };
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = [ pkgs.makeWrapper ];
          # the demo finds its assets next to the source tree; point it at the installed copy
          postInstall = ''
            mkdir -p $out/share/margin
            cp -r assets zellij skill hooks providers $out/share/margin/
            wrapProgram $out/bin/margin \
              --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.difftastic ]} --suffix PATH : ${pkgs.lib.makeBinPath [ pkgs.jq pkgs.jujutsu ]} \
              --set-default MARGIN_ASSETS $out/share/margin/assets/demo
          '';
          meta.mainProgram = "margin";
        };
      });
}
