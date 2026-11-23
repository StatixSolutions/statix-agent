{
  lib,
  rustPlatform,
  pkg-config,
  openssl,
}:

let
  sourceMetadata = builtins.fromJSON (builtins.readFile ../version.json);
  requestedVersion = builtins.getEnv "STATIX_AGENT_RELEASE_VERSION";
  requestedCommit = builtins.getEnv "STATIX_AGENT_RELEASE_COMMIT";
  version = if requestedVersion != "" then requestedVersion else sourceMetadata.version;
  commit = if requestedCommit != "" then requestedCommit else sourceMetadata.commit;
  builtAt = sourceMetadata.builtAt;
  metadata = builtins.toJSON {
    inherit version commit builtAt;
  };
in
rustPlatform.buildRustPackage {
  pname = "statix-agent";
  inherit version;

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
    install -Dm0644 <(printf '%s' '${metadata}') $out/bin/version.json
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
