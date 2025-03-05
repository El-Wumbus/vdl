use eyre::eyre;
use serde::Deserialize;
use std::fs::{self, File, Permissions};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

const NAME: &'static str = env!("CARGO_PKG_NAME");

#[derive(Clone, Debug)]
pub struct YtDlp {
    pub live_from_start:      bool,
    pub embed_metadata:       bool,
    pub embed_thumbnail:      bool,
    pub no_progress:          bool,
    pub concurrent_fragments: Option<u8>,
    pub playlist_items:       Option<u64>,
    pub remux_video:          Option<String>,
    pub cookies_from_browser: Option<String>,
}
impl Default for YtDlp {
    fn default() -> Self {
        Self {
            live_from_start:      false,
            embed_metadata:       true,
            embed_thumbnail:      true,
            no_progress:          true,
            concurrent_fragments: None,
            cookies_from_browser: None,
            remux_video:          None,
            playlist_items:       None,
        }
    }
}

impl YtDlp {
    /// Get the lastest tag
    pub fn get_latest_tag() -> eyre::Result<String> {
        #[derive(Debug, Clone, Deserialize)]
        struct Tag {
            name: String,
        }
        let body: String = ureq::get("https://api.github.com/repos/yt-dlp/yt-dlp/tags")
            .header("User-Agent", "VDL via ureq")
            .call()?
            .body_mut()
            .read_to_string()?;
        let tags = serde_json::de::from_str::<Vec<Tag>>(&body)?;
        let latest = tags
            .get(0)
            .map(|x| x.name.clone())
            .ok_or_else(|| eyre!("yt-dlp has no tags!?"))?;
        Ok(latest)
    }

    pub fn download_latest() -> eyre::Result<()> {
        let yt_dlp_exe = Self::exe_path();
        let latest = Self::get_latest_tag()?;
        let latest = latest.trim();

        if yt_dlp_exe.exists() {
            // check the version against the latest to see if we need to update it.
            let output = Command::new(&yt_dlp_exe).arg("--version").output()?;
            let stdout = String::from_utf8(output.stdout)?;
            let version = stdout.trim();
            if version == latest {
                return Ok(());
            }
            fs::remove_file(&yt_dlp_exe)?;
        }

        let url = format!(
            "https://github.com/yt-dlp/yt-dlp/releases/download/{latest}/yt-dlp_linux"
        );
        eprintln!("Downloading yt-dlp {latest} from {url:?}");
        let mut f = File::create(&yt_dlp_exe)?;
        let mut body = ureq::get(&url)
            .header("User-Agent", "VDL via ureq")
            .call()?
            .into_body();
        let mut body = body.as_reader();
        let mut buf = vec![];
        body.read_to_end(&mut buf)?;
        f.write_all(&buf)?;
        std::mem::drop(f);
        fs::set_permissions(&yt_dlp_exe, Permissions::from_mode(0o755))?;
        eprintln!("Done downloading the latest yt-dlp!");
        Ok(())
    }

    pub fn exe_path() -> PathBuf {
        let state_dir = dirs::state_dir().expect("state dir").join(NAME);
        state_dir.join("yt_dlp")
    }

    pub fn command() -> Command {
        Command::new(Self::exe_path())
    }

    pub fn command_with_args(&self) -> Command {
        let mut c = Self::command();
        c.args(self.args());
        c
    }

    pub fn concurrent_fragments(&mut self, n: Option<u8>) -> &mut Self {
        self.concurrent_fragments = n;
        self
    }
    pub fn playlist_items(&mut self, n: Option<u64>) -> &mut Self {
        self.playlist_items = n;
        self
    }
    pub fn cookies_from_browser(&mut self, browser: Option<&str>) -> &mut Self {
        self.cookies_from_browser = browser.map(str::to_string);
        self
    }
    pub fn live_from_start(&mut self, enabled: bool) -> &mut Self {
        self.live_from_start = enabled;
        self
    }
    pub fn embed_metadata(&mut self, enabled: bool) -> &mut Self {
        self.embed_metadata = enabled;
        self
    }
    pub fn embed_thumbnail(&mut self, enabled: bool) -> &mut Self {
        self.embed_thumbnail = enabled;
        self
    }
    pub fn no_progress(&mut self, no_progress: bool) -> &mut Self {
        self.no_progress = no_progress;
        self
    }
    pub fn remux_video(&mut self, format: Option<&str>) -> &mut Self {
        self.remux_video = format.map(str::to_string);
        self
    }

    fn args(&self) -> Vec<String> {
        let mut args = vec![];
        if self.live_from_start {
            args.push("--live-from-start".to_string());
        }
        if self.embed_metadata {
            args.push("--embed-metadata".to_string());
        }
        if self.embed_thumbnail {
            args.push("--embed-thumbnail".to_string());
        }
        if self.no_progress {
            args.push("--no-progress".to_string());
        }
        if let Some(n) = self.concurrent_fragments {
            args.push("--concurrent-fragments".to_string());
            args.push(n.to_string());
        }
        if let Some(n) = self.playlist_items {
            args.push("--playlist-items".to_string());
            args.push(n.to_string());
        }
        if let Some(browser) = &self.cookies_from_browser {
            args.push("--cookies-from-browser".to_string());
            args.push(browser.clone());
        }
        if let Some(format) = &self.remux_video {
            args.push("--remux-video".to_string());
            args.push(format.clone());
        }

        args
    }
}
