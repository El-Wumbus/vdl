use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Write;
use std::num::NonZeroU8;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tinyjson::{InnerAsRef, JsonValue};

const NAME: &'static str = env!("CARGO_PKG_NAME");

fn cache() -> PathBuf {
    dirs::cache_dir().expect("cache dir").join("vdl")
}

#[derive(Clone, Debug)]
struct Viewer {
    live_from_start: bool,
    embed_metadata: bool,
    embed_thumbnail: bool,
    no_progress: bool,
    concurrent_fragments: Option<NonZeroU8>,
    playlist_items: Option<u64>,
    remux_video: Option<String>,
    cookies_from_browser: Option<String>,
}
impl Default for Viewer {
    fn default() -> Self {
        Self {
            live_from_start: false,
            embed_metadata: true,
            embed_thumbnail: true,
            no_progress: true,
            concurrent_fragments: None,
            cookies_from_browser: None,
            remux_video: None,
            playlist_items: None,
        }
    }
}

impl Viewer {
    fn concurrent_fragments(&mut self, n: Option<NonZeroU8>) -> &mut Self {
        self.concurrent_fragments = n;
        self
    }
    fn playlist_items(&mut self, n: Option<u64>) -> &mut Self {
        self.playlist_items = n;
        self
    }
    fn cookies_from_browser(&mut self, browser: Option<&str>) -> &mut Self {
        self.cookies_from_browser = browser.map(str::to_string);
        self
    }
    fn live_from_start(&mut self, enabled: bool) -> &mut Self {
        self.live_from_start = enabled;
        self
    }
    fn embed_metadata(&mut self, enabled: bool) -> &mut Self {
        self.embed_metadata = enabled;
        self
    }
    fn embed_thumbnail(&mut self, enabled: bool) -> &mut Self {
        self.embed_thumbnail = enabled;
        self
    }
    fn no_progress(&mut self, no_progress: bool) -> &mut Self {
        self.no_progress = no_progress;
        self
    }
    fn remux_video(&mut self, format: Option<&str>) -> &mut Self {
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

    fn video_info(&self, id: &str) -> Result<VideoInfo, ()> {
        let url = format!("https://www.youtube.com/watch?v={id}");
        let output = Command::new("yt-dlp")
            .args(self.args())
            .arg("-J")
            .arg(url)
            .output()
            .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        if !output.status.success() || stderr.lines().any(|x| x.starts_with("ERROR")) {
            eprintln!("FAILURE:\nstderr:\n{stderr}\nstdout:\n{stdout}");
            return Err(());
        }

        let parsed: JsonValue = stdout.parse().unwrap();
        let object: &HashMap<_, _> = parsed.get().unwrap();
        let id: String = object
            .get("id")
            .and_then(String::json_value_as)
            .unwrap()
            .clone();
        let title = object
            .get("title")
            .and_then(String::json_value_as)
            .unwrap()
            .clone();
        Ok(VideoInfo { id, title })
    }

    fn live_info(&self, id: &str) -> Option<YtLiveInfo> {
        let url = format!("https://www.youtube.com/{id}/live");
        let output = Command::new("yt-dlp")
            .args(self.args())
            .arg("-J")
            .arg(url)
            .output()
            .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        if !output.status.success() || stderr.lines().any(|x| x.starts_with("ERROR")) {
            return None;
        }

        let parsed: JsonValue = stdout.parse().unwrap();
        let object: &HashMap<_, _> = parsed.get().unwrap();

        let id: String = object
            .get("id")
            .and_then(String::json_value_as)
            .unwrap()
            .clone();
        let title = object
            .get("title")
            .and_then(String::json_value_as)
            .unwrap()
            .clone();
        let is_live = object
            .get("is_live")
            .and_then(bool::json_value_as)
            .unwrap()
            .clone();
        let was_live = object
            .get("was_live")
            .and_then(bool::json_value_as)
            .unwrap()
            .clone();
        let webpage_url = object
            .get("webpage_url")
            .and_then(String::json_value_as)
            .unwrap()
            .clone();
        let uploader = object
            .get("uploader")
            .and_then(String::json_value_as)
            .unwrap()
            .clone();
        Some(YtLiveInfo {
            id,
            title,
            is_live,
            was_live,
            webpage_url,
            uploader,
        })
    }

    fn dl(&self, id: &str) -> eyre::Result<()> {
        let url = format!("https://www.youtube.com/watch?v={id}");
        let dl_dir = cache().join(id);

        let Ok(output_filename) = Command::new("yt-dlp")
            .args([&url, "--print", "filename"])
            .output()
        else {
            return Ok(());
        };
        let output_filename = String::from_utf8(output_filename.stdout)?;

        if fs::exists(&output_filename)? {
            return Ok(());
        }

        let stdout_path = dl_dir.join("yt-dlp-stdout.log");
        let stderr_path = dl_dir.join("yt-dlp-stderr.log");
        if fs::exists(&dl_dir).is_ok_and(|x| x == true) {
            fs::remove_dir_all(&dl_dir)?;
        }
        fs::create_dir_all(&dl_dir)?;
        let stdout = match File::create(&stdout_path) {
            Ok(x) => Stdio::from(x),
            Err(_) => Stdio::null(),
        };
        let stderr = match File::create(&stderr_path) {
            Ok(x) => Stdio::from(x),
            Err(_) => Stdio::null(),
        };

        println!(
            "Downloading {id} from YouTube. See {stdout_path:?} and {stderr_path:?} for logs."
        );
        let status = Command::new("yt-dlp")
            .current_dir(&dl_dir)
            .args(self.args())
            .arg(url)
            .stdout(stdout)
            .stderr(stderr)
            .status()?;

        // TODO: check error types and report errors well
        if !status.success() && !fs::exists(&output_filename).is_ok_and(|x| x) {
            eprintln!(
                "Error: Failed to download YouTube video with id \"{id}\". Check log files: \"{stdout_path:?}\" and \"{stderr_path:?}\""
            );
        }

        fs::rename(dl_dir.join(&output_filename), output_filename)?;
        fs::remove_dir_all(dl_dir)?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct VideoInfo {
    id: String,
    title: String,
}

#[derive(Debug, Clone)]
struct YtLiveInfo {
    id: String,
    title: String,
    is_live: bool,
    was_live: bool,
    webpage_url: String,
    uploader: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Id {
    Yt(String),
}

#[derive(Debug, Default)]
struct Subscriber {
    // YouTube Channel URLs
    pub list: Arc<Mutex<SubscriberList>>,
    pub watching: HashMap<String, std::thread::JoinHandle<eyre::Result<()>>>,
    pub downloaded: HashSet<String>,
}

#[derive(Debug, Default)]
struct SubscriberList {
    // A list of YouTube channel URls
    pub yt: HashSet<String>,
}

impl Subscriber {
    fn spawn(mut self) -> eyre::Result<()> {
        let mut yt = Viewer::default();
        yt.live_from_start(true)
            .remux_video(Some("mkv"))
            .cookies_from_browser(Some("firefox"));

        // Finish unfinished streams that may've ended
        for entry in fs::read_dir(cache())? {
            let Ok(entry) = entry else {
                continue;
            };
            let id = entry.file_name();
            let id = id.to_string_lossy().to_string();
            let t = std::thread::spawn({
                let id = id.clone();
                let yt = yt.clone();
                move || yt.dl(&id)
            });
            self.watching.insert(id, t);
        }

        loop {
            let list = self.list.lock().unwrap();

            let mut remove = vec![];
            for (id, task) in self.watching.iter() {
                if task.is_finished() {
                    remove.push(id.clone());
                }
            }
            for r in remove {
                println!("Finished downloading {r}!");
                if let Err(e) = self
                    .watching
                    .remove(&r)
                    .unwrap()
                    .join()
                    .expect("Download thread shouldn't panic")
                {
                    eprintln!("Download error: {e}");
                }
                self.downloaded.insert(r);
            }

            for item in list.yt.iter() {
                if let Some(info) = yt.live_info(item) {
                    if info.is_live
                        && !self.watching.contains_key(&info.id)
                        && !self.downloaded.contains(&info.id)
                    {
                        println!(
                            "{} is live!\nDownloading \"{}\"",
                            info.uploader, info.title
                        );
                        let t = std::thread::spawn({
                            let id = info.id.clone();
                            let yt = yt.clone();
                            move || yt.dl(&id)
                        });
                        self.watching.insert(info.id, t);
                    }
                }
            }
            std::mem::drop(list);
            std::thread::sleep(Duration::from_secs(60));
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Config {
    /// YouTube ids
    yt: HashSet<String>,
}

impl Config {
    fn load(path: &Path) -> eyre::Result<Self> {
        let config = if path.exists() {
            let toml = fs::read_to_string(path)?;
            basic_toml::from_str(&toml)?
        } else {
            if let Some(parent) = path.parent() {
                if !parent.exists() {
                    fs::create_dir_all(&parent)?;
                }
            }
            let config = Config::default();
            let toml = basic_toml::to_string(&config)?;
            let mut f = File::create(path)?;
            f.write_all(toml.as_bytes())?;
            config
        };
        Ok(config)
    }
}

fn main() -> eyre::Result<()> {
    let config_path = dirs::config_dir()
        .expect("config dir")
        .join(NAME)
        .join("config.toml");
    let config = Config::load(&config_path)?;

    // TODO: config reloading

    let subscriber = Subscriber::default();
    let sublist = subscriber.list.clone();
    {
        let mut list = sublist.lock().unwrap();
        *list = SubscriberList { yt: config.yt };
    }
    subscriber.spawn()?;
    loop {
        std::thread::sleep(Duration::from_secs(1024));
    }
}
