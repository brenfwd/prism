Files for packaging and installing Prism.

- `prism.service` is a systemd unit. Copy it to `/etc/systemd/system/`, adjust `ReadWritePaths` for pools outside `/var/lib/prism`, then run `systemctl daemon-reload` and `systemctl enable --now prism`.
- `prism.toml.example` lists every `prism.toml` option with its default, all commented out.

The unit expects the binary at `/usr/local/bin/prism` and the config directory at `/etc/prism`. The [operations guide](../docs/guide/operations.md) describes both.
