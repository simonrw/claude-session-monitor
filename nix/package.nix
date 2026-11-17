{
  lib,
  rustPlatform,
  stdenv,
  makeWrapper,
  bun2nix,
}:
let
  web = stdenv.mkDerivation {
    pname = "csm-web";
    version = (lib.importJSON ../web/package.json).version;

    src = ../web;

    nativeBuildInputs = [ bun2nix.hook ];
    bunDeps = bun2nix.fetchBunDeps { bunNix = ../web/bun.nix; };

    buildPhase = "bun run build";
    installPhase = "cp --recursive dist $out";
  };
in
rustPlatform.buildRustPackage {
  pname = "csm-server";
  version = (lib.importTOML ../crates/server/Cargo.toml).package.version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../crates
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;
  cargoBuildFlags = [
    "--package"
    "csm-server"
  ];

  # The repo's CI runs the workspace tests.
  doCheck = false;

  nativeBuildInputs = [ makeWrapper ];
  postInstall = ''
    wrapProgram $out/bin/csm-server --add-flags "--static-dir ${web}"
  '';

  passthru = { inherit web; };

  meta.mainProgram = "csm-server";
}
