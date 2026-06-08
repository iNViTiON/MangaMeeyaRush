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

        # Build mmce with the same pinned toolchain as the dev shell.
        rustPlatform = pkgs.makeRustPlatform {
          cargo = toolchain;
          rustc = toolchain;
        };

        runtimeLibs = with pkgs; [
          # egui / winit / wgpu runtime deps — these are dlopen'd at runtime,
          # not recorded in the ELF, so they must be on LD_LIBRARY_PATH for
          # both the dev shell and the wrapped package binary.
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

        libraryPath = pkgs.lib.makeLibraryPath runtimeLibs;

        # Keep the source tree small: drop build/output/reference dirs that the
        # crate build never reads (.cargo/config.toml IS kept — it carries the
        # target-cpu=native SIMD flags).
        src = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter = path: type:
            let rel = pkgs.lib.removePrefix (toString ./. + "/") (toString path);
            in !(pkgs.lib.hasPrefix "target" rel
              || pkgs.lib.hasPrefix "outputs" rel
              || pkgs.lib.hasPrefix "legacy" rel);
        };

        mmce = rustPlatform.buildRustPackage {
          pname = "mmce";
          version = "0.1.0";
          inherit src;

          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = with pkgs; [
            pkg-config
            cmake
            makeWrapper
          ];

          # Build-time linkage (pkg-config / -l): fontconfig, freetype, libbz2.
          buildInputs = with pkgs; [
            fontconfig
            freetype
            bzip2
          ];

          # GUI + workspace tests need a display; skip them in the sandbox.
          doCheck = false;

          # Inject the runtime dlopen path so `mmce` runs from anywhere, not
          # just inside the dev shell.
          postInstall = ''
            wrapProgram $out/bin/mmce \
              --prefix LD_LIBRARY_PATH : "${libraryPath}"
          '';

          meta = with pkgs.lib; {
            description = "Cross-platform Rust port of the MangaMeeya CE manga reader";
            license = licenses.mit;
            mainProgram = "mmce";
            platforms = platforms.linux;
          };
        };
      in
      {
        packages.default = mmce;
        packages.mmce = mmce;

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

          LD_LIBRARY_PATH = libraryPath;
          RUST_BACKTRACE = "1";

          shellHook = ''
            echo "mmce dev shell — $(rustc --version)"
          '';
        };
      });
}
