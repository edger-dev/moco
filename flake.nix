{
  description = "moco — composable plugin framework";

  inputs = {
    jig.url = "github:edger-dev/jig";
  };

  outputs = { self, jig }:
    jig.lib.mkWorkspace
      {
        pname = "moco";
        src = ./.;
        extraDevPackages = pkgs: [
          pkgs.openssl
          pkgs.libxkbcommon
          pkgs.libGL
          pkgs.wayland
          pkgs.skia
          pkgs.freetype
          pkgs.fontconfig
        ];
      }
      {
        rust = { wasm = true; };
        docs = { beans = true; };
      };
}
