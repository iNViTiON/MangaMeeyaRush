{
  description = "MangaMeeya Rust — cross-platform manga image viewer";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, fenix }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        toolchain = fenix.packages.${system}.stable.toolchain;

        runtimeLibs = with pkgs; [
          # egui / winit / wgpu runtime deps
          libxkbcommon
          libGL
          wayland
          libx11
          libxcursor
          libxi
          libxrandr
          libxcb
          vulkan-loader
          fontconfig
          freetype
          bzip2
        ];
      in
      {
        devShells.default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            toolchain
            pkg-config
            cmake
            xvfb-run
          ];

          buildInputs = runtimeLibs ++ (with pkgs; [
            fontconfig
            freetype
          ]);

          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs;
          RUST_BACKTRACE = "1";

          shellHook = ''
            echo "mmce dev shell — $(rustc --version)"
          '';
        };
      });
}
