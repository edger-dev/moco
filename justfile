project_root := justfile_directory()

show_flake_outputs:
  om show {{project_root}}

start-rustfs:
  RUSTFS_ACCESS_KEY="moco" \
  RUSTFS_SECRET_KEY="roco" \
  RUSTFS_ADDRESS=":8881" \
  RUSTFS_CONSOLE_ADDRESS=":8882" \
  RUSTFS_CONSOLE_ENABLE="true" \
  RUSTFS_OBS_LOG_DIRECTORY="{{ project_root }}/local/rustfs/logs" \
  {{ project_root }}/bin/rustfs "{{ project_root }}/local/rustfs/volumns"

rc-set-local:
  rc alias set local http://localhost:8881 moco roco

rc-make-local-buckets:
  rc ls local/
  rc mb local/s2lite
  rc ls local/

start-s2lite:
  AWS_ACCESS_KEY_ID="moco" \
  AWS_SECRET_ACCESS_KEY="roco" \
  AWS_ENDPOINT_URL_S3="http://127.0.0.1:8881" \
  AWS_ALLOW_HTTP="true" \
  {{ project_root }}/bin/server --bucket s2lite --port 8883

start-openobserve:
  openobserve

install-tools:
  @just _install_cargo_tool rustfs --git https://github.com/rustfs/rustfs
  @just _install_cargo_tool rustfs-cli
  @just _install_cargo_tool s2-lite
  @just _install_cargo_tool s2-cli
  @just _install_cargo_tool styx-cli

_install_cargo_tool *ARGS:
  cargo install --locked --root {{ project_root }} {{ARGS}}
