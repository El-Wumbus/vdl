use eyre::eyre;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use signal_hook::consts::SIGHUP;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Write;
use std::num::NonZeroU8;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use clap::Parser;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

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

    /*fn video_info(&self, id: &str) -> Result<VideoInfo, ()> {
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
    }*/

    fn live_info(&self, id: &str) -> eyre::Result<Option<YtLiveInfo>> {
        let url = format!("https://www.youtube.com/{id}/live");
        let output = Command::new("yt-dlp")
            .args(self.args())
            .arg("-J")
            .arg(url)
            .output()?;
        let stdout = String::from_utf8(output.stdout)?;
        let stdout = stdout.trim();
        if stdout.is_empty() {
            return Ok(None);
        }
        let info: YtLiveInfo = serde_json::from_str(&stdout)?;
        Ok(Some(info))
    }

    fn dl(&self, url: &str, dl_dir: PathBuf) -> eyre::Result<()> {
        let current_dir = std::env::current_dir()?;
        let Ok(output) = Command::new("yt-dlp")
            .args([&url, "--print", "_filename"])
            .output()
        else {
            return Ok(());
        };
        let stdout = String::from_utf8(output.stdout)?;
        let stdout = stdout.trim();
        if stdout.is_empty() {
            return Err(eyre!("Filename is empty!"));
        }
        let mut output_filename = PathBuf::from(stdout);
        if let Some(format) = self.remux_video.as_deref() {
            output_filename.set_extension(format);
        }
        let final_out = current_dir.join(&output_filename);
        let tmp_out_path = dl_dir.join(&output_filename);

        if fs::exists(&final_out)? {
            return Ok(());
        }
        if tmp_out_path.exists() {
            fs::rename(&tmp_out_path, &final_out)
                .map_err(|e| eyre!("{e}: {tmp_out_path:?} -> {final_out:?}"))?;

            fs::remove_dir_all(dl_dir)?;
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

        let _status = Command::new("yt-dlp")
            .current_dir(&dl_dir)
            .args(self.args())
            // yt-dlp often doesn't write to what it says it will, so that's why I must
            // remind it to.
            .args([&url, "--output", tmp_out_path.to_str().unwrap()])
            .stdout(stdout)
            .stderr(stderr)
            .status()?;

        fs::rename(&tmp_out_path, &final_out)
            .map_err(|e| eyre!("{e}: {tmp_out_path:?} -> {final_out:?}"))?;
        fs::remove_dir_all(dl_dir)?;
        Ok(())
    }

    fn yt_dl(&self, id: &str) -> eyre::Result<()> {
        let url = format!("https://www.youtube.com/watch?v={id}");
        let dl_dir = cache().join(id);
        self.dl(&url, dl_dir)
    }

    fn twitch_is_live(id: &str) -> bool {
        let url = format!("https://www.twitch.tv/{id}");
        let Ok(output) = Command::new("yt-dlp")
            .args([&url, "--quiet", "--simulate"])
            .output()
        else {
            return false;
        };

        String::from_utf8(output.stderr)
            .is_ok_and(|x| !x.contains("The channel is not currently live"))
    }

    fn twitch_dl(&self, id: &str) -> eyre::Result<()> {
        let url = format!("https://www.twitch.tv/{id}");
        let dl_dir = cache().join(format!("twitch:{id}"));
        self.dl(&url, dl_dir)
    }
}

#[derive(Debug, Clone)]
struct VideoInfo {
    id: String,
    title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct YtLiveInfo {
    id: String,
    title: String,
    is_live: bool,
    was_live: bool,
    webpage_url: String,
    uploader: String,
}

#[derive(Debug, Default)]
struct SubscriberList {
    // A list of YouTube channel URls
    pub yt: HashSet<String>,
    // A list of Twitch channel names/IDs
    pub twitch: HashSet<String>,
}

// TODO:
#[derive(Debug, Clone, PartialEq)]
enum Id {
    Yt(String),
    Twitch(String),
}

#[derive(Debug, Default)]
struct Subscriber {
    // YouTube Channel URLs
    pub list: Arc<Mutex<SubscriberList>>,

    pub watching: HashMap<String, std::thread::JoinHandle<eyre::Result<()>>>,
    pub downloaded: HashSet<String>,
    multi_progress: MultiProgress,
    progress_bars: HashMap<String, ProgressBar>,
}

impl Subscriber {
    pub fn spawn(mut self, silent: bool) -> eyre::Result<()> {
        fn pbar() -> ProgressBar {
            let pb = ProgressBar::new_spinner().with_elapsed(Duration::ZERO);
            pb.set_style(
                ProgressStyle::with_template(
                    "{spinner:.green} {msg} [{elapsed_precise}]",
                )
                .unwrap(),
            );
            pb
        }

        let mut yt = Viewer::default();
        yt.live_from_start(true)
            .remux_video(Some("mkv"))
            .cookies_from_browser(Some("firefox"));

        let mut twitch = Viewer::default();
        twitch
            .remux_video(Some("mkv"))
            .cookies_from_browser(Some("firefox"));

        let cache_dir = cache();
        if !cache_dir.exists() {
            fs::create_dir_all(&cache_dir)?;
        }
        // Finish unfinished streams that may've ended
        for entry in fs::read_dir(&cache_dir)? {
            let Ok(entry) = entry else {
                continue;
            };
            let id = entry.file_name();
            let id = id.to_string_lossy().to_string();
            if let Some(id) = id.strip_prefix("twitch:") {
                let tid = format!("twitch:{id}");
                let t = std::thread::spawn({
                    let id = id.to_string();
                    let twitch = twitch.clone();
                    move || twitch.twitch_dl(&id)
                });
                self.watching.insert(tid.clone(), t);
                if !silent {
                    let pb = self.multi_progress.add(pbar());
                    pb.set_message(format!("{id}'s Twitch stream"));
                    self.progress_bars.insert(tid, pb);
                }
            } else {
                let t = std::thread::spawn({
                    let id = id.to_string();
                    let yt = yt.clone();
                    move || yt.yt_dl(&id)
                });
                self.watching.insert(id.clone(), t);
                if !silent {
                    let pb = self.multi_progress.add(pbar());
                    pb.set_message(format!("Resuming YouTube download: {id:?}"));
                    self.progress_bars.insert(id, pb);
                }
            }
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
                let pb = self.progress_bars.remove(&r).unwrap();
                if let Err(e) = self
                    .watching
                    .remove(&r)
                    .unwrap()
                    .join()
                    .expect("Download thread shouldn't panic")
                {
                    pb.finish_with_message(format!("Download error: {e}"));
                } else {
                    pb.finish_with_message("Downloaded!");
                }

                self.downloaded.insert(r);
            }

            for id in list.twitch.iter() {
                let tid = format!("twitch:{id}");
                if Viewer::twitch_is_live(id)
                    && !self.watching.contains_key(&tid)
                    && !self.downloaded.contains(&tid)
                {
                    println!("Downloading {id}'s  livestream!",);
                    let t = std::thread::spawn({
                        let id = id.clone();
                        let twitch = twitch.clone();
                        move || twitch.twitch_dl(&id)
                    });
                    self.watching.insert(tid.clone(), t);
                    if !silent {
                        let pb = self.multi_progress.add(pbar());
                        pb.set_message(format!("{id}'s Twitch stream"));
                        self.progress_bars.insert(tid, pb);
                    }
                }
            }

            for item in list.yt.iter() {
                if let Ok(Some(info)) = yt.live_info(item) {
                    if info.is_live
                        && !self.watching.contains_key(&info.id)
                        && !self.downloaded.contains(&info.id)
                    {
                        let t = std::thread::spawn({
                            let id = info.id.clone();
                            let yt = yt.clone();
                            move || yt.yt_dl(&id)
                        });
                        self.watching.insert(info.id.clone(), t);
                        if !silent {
                            let pb = self.multi_progress.add(pbar());
                            pb.set_message(format!("{:?} by {}", info.title, info.uploader));
                            self.progress_bars.insert(info.id, pb);
                        }
                    }
                }
            }
            std::mem::drop(list);

            for _ in 0..(30 * 1000 / 100) {
                std::thread::sleep(Duration::from_millis(100));
                self.progress_bars.values().for_each(|pb| pb.tick());
            }
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Config {
    dir: Option<PathBuf>,
    /// YouTube ids
    #[serde(default)]
    yt: HashSet<String>,
    #[serde(default)]
    twitch: HashSet<String>,
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

#[derive(Debug, clap::Parser)]
#[command(version, about)]
struct Args {
    #[arg(short, long)]
    silent: bool
}

fn main() -> eyre::Result<()> {
    let exit = Arc::new(AtomicBool::new(false));
    let reload_config = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(SIGHUP, reload_config.clone()).unwrap();

    let args = Args::parse();
    let config_path = dirs::config_dir()
        .expect("config dir")
        .join(NAME)
        .join("config.toml");
    let mut config = Config::load(&config_path)?;
    if let Some(dir) = config.dir.as_deref() {
        std::env::set_current_dir(dir).map_err(|e| eyre!("{dir:?}: {e}"))?;
    }

    let subscriber = Subscriber::default();
    let sublist = subscriber.list.clone();
    {
        let mut list = sublist.lock().unwrap();
        *list = SubscriberList {
            yt: config.yt,
            twitch: config.twitch,
        };
    }
    
    let subscriber = std::thread::spawn( move || subscriber.spawn(args.silent));
    
    loop {
        if subscriber.is_finished() {
            eprintln!("Subscriber exited!");
            subscriber.join().unwrap()?;
            std::process::exit(1);
        }
        if reload_config.swap(false, Ordering::Relaxed) {
            eprintln!("Reloading config..");
            match Config::load(&config_path) {
                Ok(c) => {
                    eprintln!("Reloaded config!");
                    let old_dir = config.dir;
                    config = c;
                    let mut list = sublist.lock().unwrap();
                    *list = SubscriberList {
                        yt: config.yt,
                        twitch: config.twitch,
                    };
                    if config.dir.is_some() && config.dir != old_dir {
                        let dir = config.dir.as_deref().unwrap();
                        std::env::set_current_dir(dir)
                            .map_err(|e| eyre!("{dir:?}: {e}"))?;
                    }
                }
                Err(e) => {
                    eprintln!("Failed to reload state (retaining previous state): {e}")
                }
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}
