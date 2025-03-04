Download YouTube and Twitch streams as they go live. Never miss a livestream again.

## Install

`vdl` can be installed with cargo.

```bash
cargo install --git 'https://github.com/El-Wumbus/vdl'
```

## Configuration

Before `vdl` can be used, it must be configured.  
The configuration file is located at `XDG_CONFIG_HOME/vdl/config.toml` or
`~/.config/vdl/config.toml` and looks like the following:

```toml
dir = "/home/user/Video" # default: ~/Videos 
[[ids]]
yt_id = "@PiscosHour"
[[twitch]]
twitch_id = "theprimeagen"
```

To reload the configuration file without restarting the server, hit the
progress with a SIGHUP: `pkill -1 vdl`.
