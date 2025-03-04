use clap::{Parser, Subcommand};
use eyre::eyre;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use signal_hook::consts::{SIGHUP, SIGTERM};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::Shutdown;
use std::num::NonZeroU8;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const NAME: &'static str = env!("CARGO_PKG_NAME");

#[derive(Clone, Debug)]
struct Viewer {
    live_from_start:      bool,
    embed_metadata:       bool,
    embed_thumbnail:      bool,
    no_progress:          bool,
    concurrent_fragments: Option<NonZeroU8>,
    playlist_items:       Option<u64>,
    remux_video:          Option<String>,
    cookies_from_browser: Option<String>,
}
impl Default for Viewer {
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
            .args(self.args())
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
            .args([
                &url,
                "--output",
                tmp_out_path.to_str().unwrap(),
                "--write-info-json",
            ])
            .stdout(stdout)
            .stderr(stderr)
            .status()?;

        fs::rename(&tmp_out_path, &final_out)
            .map_err(|e| eyre!("{e}: {tmp_out_path:?} -> {final_out:?}"))?;
        fs::remove_dir_all(dl_dir)?;
        Ok(())
    }

    fn yt_dl(&self, id: &str, dl_dir: PathBuf) -> eyre::Result<()> {
        let url = format!("https://www.youtube.com/watch?v={id}");
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

    fn twitch_dl(&self, id: &str, dl_dir: PathBuf) -> eyre::Result<()> {
        let url = format!("https://www.twitch.tv/{id}");
        self.dl(&url, dl_dir)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct YtLiveInfo {
    id:          String,
    title:       String,
    is_live:     bool,
    was_live:    bool,
    webpage_url: String,
    uploader:    String,
}

// TODO:
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
enum Id {
    Yt { yt_id: String },
    Twitch { twitch_id: String },
}
impl Id {
    fn as_str(&self) -> &str {
        match self {
            Id::Yt { yt_id } => yt_id,
            Id::Twitch { twitch_id } => twitch_id,
        }
    }
}

impl std::fmt::Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Id::Yt { yt_id } => write!(f, "yt:{yt_id}"),
            Id::Twitch { twitch_id } => write!(f, "twitch:{twitch_id}"),
        }
    }
}

impl std::str::FromStr for Id {
    type Err = eyre::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(s) = s.strip_prefix("yt:") {
            Ok(Self::Yt {
                yt_id: s.to_string(),
            })
        } else if let Some(s) = s.strip_prefix("twitch:") {
            Ok(Self::Twitch {
                twitch_id: s.to_string(),
            })
        } else {
            Err(eyre!("{s} is not a valid id"))
        }
    }
}

#[derive(Debug, Default)]
struct InnerSub {
    pub ids:        HashSet<Id>,
    pub watching:   HashMap<Id, std::thread::JoinHandle<eyre::Result<()>>>,
    pub downloaded: HashSet<Id>,
}

#[derive(Debug, Default)]
struct Subscriber {
    // YouTube Channel URLs
    pub inner: Arc<Mutex<InnerSub>>,

    progress_bars:  HashMap<Id, ProgressBar>,
    multi_progress: MultiProgress,
}

impl Subscriber {
    pub fn spawn(mut self, silent: bool) -> eyre::Result<()> {
        let cache_dir = cache();
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

        if !cache_dir.exists() {
            fs::create_dir_all(&cache_dir)?;
        }

        let mut inner = self.inner.lock().unwrap();
        // Handle unfinished downloads
        for entry in fs::read_dir(&cache_dir)? {
            let Ok(entry) = entry else {
                continue;
            };
            let id = entry.file_name();
            let id = id.to_string_lossy().to_string();

            let Ok(id) = id.parse::<Id>() else { continue };
            let dl_dir = cache_dir.join(id.to_string());
            let t = match id.clone() {
                Id::Twitch { twitch_id } => {
                    let t = std::thread::spawn({
                        let twitch = twitch.clone();
                        let twitch_id = twitch_id.clone();
                        move || twitch.twitch_dl(&twitch_id, dl_dir)
                    });
                    if !silent {
                        let pb = pbar();
                        pb.set_message(format!("{twitch_id}'s Twitch stream"));
                        let pb = self.multi_progress.add(pb);
                        self.progress_bars.insert(id.clone(), pb);
                    }
                    t
                }
                Id::Yt { yt_id } => {
                    let t = std::thread::spawn({
                        let yt_id = yt_id.clone();
                        let yt = yt.clone();
                        move || yt.yt_dl(&yt_id, dl_dir)
                    });
                    if !silent {
                        let pb = pbar();
                        pb.set_message(format!("Resuming YouTube video: {yt_id}"));
                        let pb = self.multi_progress.add(pb);
                        self.progress_bars.insert(id.clone(), pb);
                    }
                    t
                }
            };
            inner.watching.insert(id.clone(), t);
        }
        std::mem::drop(inner);

        loop {
            let mut inner = self.inner.lock().unwrap();

            let mut remove = vec![];
            for (id, task) in inner.watching.iter() {
                if task.is_finished() {
                    remove.push(id.clone());
                }
            }

            for r in remove {
                let message = if let Err(e) = inner
                    .watching
                    .remove(&r)
                    .unwrap()
                    .join()
                    .expect("Download thread shouldn't panic")
                {
                    format!("Download error: {e}",)
                } else {
                    "Downloaded!".to_string()
                };

                if !silent {
                    let pb = self.progress_bars.remove(&r).unwrap();
                    pb.finish_with_message(message);
                }

                inner.downloaded.insert(r);
            }

            for id in inner.ids.clone() {
                match &id {
                    Id::Yt { yt_id } => {
                        let Ok(Some(info)) = yt.live_info(&yt_id) else {
                            continue;
                        };
                        let dl_dir = cache_dir.join(Id::Yt {yt_id: info.id.clone()}.to_string());
                        let video_id = Id::Yt {
                            yt_id: info.id.clone(),
                        };
                        if info.is_live
                            && !inner.watching.contains_key(&video_id)
                            && !inner.downloaded.contains(&video_id)
                        {
                            let thread = std::thread::spawn({
                                let id = info.id.clone();
                                let yt = yt.clone();
                                move || yt.yt_dl(&id, dl_dir)
                            });
                            inner.watching.insert(video_id.clone(), thread);
                            if !silent {
                                let pb = self.multi_progress.add(pbar());
                                pb.set_message(format!(
                                    "{:?} by {}",
                                    info.title, info.uploader
                                ));
                                self.progress_bars.insert(video_id, pb);
                            }
                        }
                    }
                    Id::Twitch { twitch_id }
                        if Viewer::twitch_is_live(&twitch_id)
                            && !inner.watching.contains_key(&id)
                            && !inner.downloaded.contains(&id) =>
                    {
                        let dl_dir = cache_dir.join(id.to_string());
                        let thread = std::thread::spawn({
                            let twitch_id = twitch_id.clone();
                            let twitch = twitch.clone();
                            move || twitch.twitch_dl(&twitch_id, dl_dir)
                        });
                        inner.watching.insert(id.clone(), thread);
                        if !silent {
                            let pb = self.multi_progress.add(pbar());
                            pb.set_message(format!("{id}'s Twitch stream"));
                            self.progress_bars.insert(id.clone(), pb);
                        }
                    }
                    _ => {}
                }
            }
            std::mem::drop(inner);

            for _ in 0..(45 * 1000 / 100) {
                std::thread::sleep(Duration::from_millis(100));
                self.progress_bars.values().for_each(|pb| pb.tick());
            }
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Config {
    dir: Option<PathBuf>,
    #[serde(default)]
    ids: HashSet<Id>,
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

#[derive(Debug, Deserialize, Serialize)]
enum IpcRequest {
    GetWatching,
}

#[derive(Debug, Deserialize, Serialize)]
enum IpcResponse {
    Watching(Vec<Id>),
    Error(String),
}

struct Ipc {
    inner_sub: Arc<Mutex<InnerSub>>,
    listener:  UnixListener,
}

impl Ipc {
    fn new(inner_sub: Arc<Mutex<InnerSub>>) -> eyre::Result<Self> {
        let state_dir = dirs::state_dir().expect("User state dir").join(NAME);
        let socket = state_dir.join("ipc.sock");

        if !state_dir.exists() {
            fs::create_dir_all(&state_dir)?;
        }
        if socket.exists() {
            fs::remove_file(&socket).map_err(|e| {
                eyre!("Failed to remove old socket, maybe a server is running?\n{e}")
            })?;
        }
        Ok(Self {
            inner_sub,
            listener: UnixListener::bind(&socket)?,
        })
    }

    fn spawn(self) -> eyre::Result<()> {
        let mut message_body = Vec::new();
        loop {
            let (mut stream, _sock_addr) = self.listener.accept()?;

            message_body.clear();
            stream.read_to_end(&mut message_body)?;

            let response = match serde_json::de::from_slice(&message_body) {
                Ok(request) => self.handle_request(request),
                Err(e) => IpcResponse::Error(format!(
                    "Error: failed to parse JSON request: {e}"
                )),
            };
            let response_json = match serde_json::ser::to_vec(&response) {
                Ok(x) => x,
                Err(e) => serde_json::ser::to_vec(&IpcResponse::Error(format!(
                    "Error: failed to serialize response: {e}"
                )))
                .unwrap(),
            };
            stream.write_all(&response_json)?;
        }
    }

    fn handle_request(&self, req: IpcRequest) -> IpcResponse {
        match req {
            IpcRequest::GetWatching => {
                let inner = self.inner_sub.lock().unwrap();
                let watching = inner.watching.keys().cloned().collect::<Vec<_>>();
                IpcResponse::Watching(watching)
            }
        }
    }
}

#[derive(Debug, Parser)]
#[command(version, about)]
enum Args {
    /// Spawn the VOD downloading server.
    Watch {
        #[arg(short, long)]
        silent: bool,
    },

    /// Communicate with the locally running VDL server.
    Ipc {
        #[command(subcommand)]
        subcommand: IpcCommand,
    },
    /// Write shell-completions and exit.
    Completions,
}

#[derive(Debug, Subcommand)]
enum IpcCommand {
    /// Find out what streams the server is currently downloading, if any.
    GetWatching,
}

fn main() -> eyre::Result<()> {
    let command = Args::parse();
    match command {
        Args::Watch { silent } => serve(silent),
        Args::Ipc { subcommand } => ipc(subcommand),
        Args::Completions => {
            use clap_complete::Shell;
            use clap::CommandFactory;
            let mut cmd = Args::command();
            let shell = Shell::from_env().ok_or_else(||eyre!("Couldn't determine shell from environment!"))?;
            
            match shell {
                Shell::Fish => {
                    let vendor_completions_dir = dirs::data_dir().expect("data dir").join("fish/vendor_completions.d");
                    let vendor_completions_path = vendor_completions_dir.join("vdl.fish");
                    if !vendor_completions_dir.exists() {
                        fs::create_dir_all(&vendor_completions_dir)?;
                    }
                    let mut f = File::create(&vendor_completions_path)?;
                    eprintln!("Writing completions to {vendor_completions_path:?}");
                    clap_complete::generate(shell, &mut cmd, NAME, &mut f);
                }
                _=> {
                    clap_complete::generate(shell, &mut cmd, NAME, &mut std::io::stdout());
                }
            };
            Ok(())       
        }
    }
}

fn ipc(command: IpcCommand) -> eyre::Result<()> {
    let state_dir = dirs::state_dir().expect("User state dir").join(NAME);
    let socket = state_dir.join("ipc.sock");
    let mut stream = UnixStream::connect(&socket).map_err(|e| {
        eyre!(
            "Couldn't connect to socket {socket:?} (ensure an instance is running): {e}"
        )
    })?;

    let request = match command {
        IpcCommand::GetWatching => IpcRequest::GetWatching,
    };
    let request_json = serde_json::ser::to_vec(&request)?;
    stream.write(&request_json)?;
    stream.shutdown(Shutdown::Write)?;

    let mut response_json = Vec::new();
    stream.read_to_end(&mut response_json)?;

    let response: IpcResponse = serde_json::de::from_slice(&response_json)?;
    match response {
        IpcResponse::Watching(watching) => {
            let twitch = watching
                .iter()
                .filter(|x| matches!(x, Id::Twitch { .. }))
                .map(Id::as_str);
            let yt = watching
                .iter()
                .filter(|x| matches!(x, Id::Yt { .. }))
                .map(Id::as_str);

            // TODO: Tabular display
            println!("Watching {} streams", watching.len());
            if !watching.is_empty() {
                for (i, id) in twitch.enumerate() {
                    if i == 0 {
                        println!("Twitch");
                    }
                    println!(":: {id}");
                }
                for (i, id) in yt.enumerate() {
                    if i == 0 {
                        println!("YouTube");
                    }
                    println!(":: {id}");
                }
            }
        }
        IpcResponse::Error(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
    Ok(())
}

fn serve(silent: bool) -> eyre::Result<()> {
    let reload_config = Arc::new(AtomicBool::new(false));
    let exit = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(SIGHUP, reload_config.clone()).unwrap();
    signal_hook::flag::register(SIGTERM, exit.clone()).unwrap();

    let config_path = dirs::config_dir()
        .expect("config dir")
        .join(NAME)
        .join("config.toml");
    let mut config = Config::load(&config_path)?;
    
    if let Some(dir) = config.dir.clone().or_else(dirs::video_dir) {
        std::env::set_current_dir(&dir).map_err(|e| eyre!("{dir:?}: {e}"))?;
    }

    let subscriber = Subscriber::default();
    let inner = subscriber.inner.clone();
    {
        let mut inner = inner.lock().unwrap();
        inner.ids = config.ids;
    }
    let ipc = Ipc::new(inner.clone())?;
    std::thread::spawn(move || ipc.spawn());

    let subscriber = std::thread::spawn(move || subscriber.spawn(silent));

    loop {
        if subscriber.is_finished() {
            eprintln!("Subscriber exited!");
            subscriber.join().unwrap()?;
            std::process::exit(1);
        }
        if exit.swap(false, Ordering::Relaxed) {
            let state_dir = dirs::state_dir().expect("User state dir").join(NAME);
            let socket = state_dir.join("ipc.sock");
            let _ = fs::remove_file(socket);
            std::process::exit(1);
        }
        if reload_config.swap(false, Ordering::Relaxed) {
            eprintln!("Reloading config..");
            match Config::load(&config_path) {
                Ok(c) => {
                    eprintln!("Reloaded config!");
                    let old_dir = config.dir;
                    config = c;
                    let mut inner = inner.lock().unwrap();
                    inner.ids = config.ids;
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

fn cache() -> PathBuf {
    dirs::cache_dir().expect("cache dir").join("vdl")
}
