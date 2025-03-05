Download YouTube and Twitch streams as they go live. Never miss a livestream again.

## Dependencies

`vdl` depends on [`yt-dlp`](https://github.com/yt-dlp/yt-dlp), but it downloads
the latest version automatically on startup.

TODO: add configuration option for PATH bypass.

## Install

`vdl` can be installed with cargo.

```bash
cargo install --git 'https://github.com/El-Wumbus/vdl'
```

## Configuration

Before `vdl` can be used, it must be configured.  
The configuration file is located at `$XDG_CONFIG_HOME/vdl/config.toml` or
`~/.config/vdl/config.toml` and looks like the following:

```toml
dir = "/home/user/Video" # default: ~/Videos 
[[ids]]
yt_id = "@PiscosHour"
[[ids]]
twitch_id = "theprimeagen"
```

To reload the configuration file without restarting the server, hit the
progress with a SIGHUP: `pkill -1 vdl`.
