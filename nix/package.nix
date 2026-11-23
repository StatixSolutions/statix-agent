{
  lib,
  rustPlatform,
  pkg-config,
  openssl,
}:

rustPlatform.buildRustPackage {
  pname = "statix-agent";
  version = (builtins.fromJSON (builtins.readFile ../version.json)).version;

  src = lib.cleanSourceWith {
    src = ./..;
    filter = path: type:
      let
        baseName = baseNameOf path;
      in
      !(type == "directory" && baseName == "target")
      && !(type == "directory" && baseName == ".git");
  };

  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [
    pkg-config
  ];

  buildInputs = [
    openssl
  ];

  postInstall = ''
    install -Dm0644 version.json $out/bin/version.json
    install -Dm0755 installers/common/statix-agent-capabilities.sh \
      $out/bin/statix-agent-capabilities
    install -Dm0755 installers/common/statix-agent-lxc-helper \
      $out/bin/statix-agent-lxc
  '';

  meta = {
    description = "Statix node agent";
    homepage = "https://statix.se";
    license = lib.licenses.asl20;
    mainProgram = "statix-agent";
  };
}
