use clap::{Parser, Subcommand};
use eyre::eyre;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use signal_hook::consts::{SIGHUP, SIGTERM};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[allow(dead_code)]
mod yt_dlp;

use yt_dlp::*;

const NAME: &'static str = env!("CARGO_PKG_NAME");

#[derive(Debug)]
struct Watching {
    thread: std::thread::JoinHandle<eyre::Result<()>>,
    info:   Info,
    dl_dir: PathBuf,
}

impl Watching {
    fn watch(mut yt_dlp: YtDlp, id: &Id) -> eyre::Result<Self> {
        let dl_dir = dirs::cache_dir()
            .expect("cache dir")
            .join(NAME)
            .join(id.to_string());
        let info = Info::get(&yt_dlp, &id)?;
        let thread = match id {
            Id::Twitch { twitch_id } => {
                let t = std::thread::spawn({
                    let twitch_id = twitch_id.clone();
                    let dl_dir = dl_dir.clone();
                    move || twitch_dl(&yt_dlp, &twitch_id, dl_dir)
                });
                t
            }
            Id::Yt { yt_id } => {
                yt_dlp.live_from_start(true);
                let t = std::thread::spawn({
                    let yt_id = yt_id.clone();
                    let dl_dir = dl_dir.clone();
                    move || yt_dl(&yt_dlp, &yt_id, dl_dir)
                });
                t
            }
        };

        Ok(Self {
            thread,
            info,
            dl_dir,
        })
    }
}

#[derive(Debug, Default)]
struct InnerSub {
    pub ids: HashSet<Id>,

    pub watching:   HashMap<Id, Watching>,
    pub downloaded: HashMap<Id, Info>,
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
        let cache_dir = dirs::cache_dir().expect("cache dir").join(NAME);
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

        let mut yt_dlp = YtDlp::default();
        yt_dlp
            .concurrent_fragments(Some(2))
            .remux_video(Some("mkv"))
            .cookies_from_browser(Some("firefox"));

        if !cache_dir.exists() {
            fs::create_dir_all(&cache_dir)?;
        }

        // Handle unfinished downloads
        for entry in fs::read_dir(&cache_dir)? {
            let mut inner = self.inner.lock().unwrap();

            let Ok(entry) = entry else {
                continue;
            };
            let id = entry.file_name();
            let id = id.to_string_lossy().to_string();

            let Ok(id) = id.parse::<Id>() else { continue };
            let Ok(watching) = Watching::watch(yt_dlp.clone(), &id) else {
                continue;
            };
            if !silent {
                let pb = pbar();
                pb.set_message(id.to_string());
                let pb = self.multi_progress.add(pb);
                self.progress_bars.insert(id.clone(), pb);
            }
            inner.watching.insert(id.clone(), watching);
        }

        loop {
            let mut inner = self.inner.lock().unwrap();
            let mut remove = vec![];
            for (id, task) in inner.watching.iter() {
                if task.thread.is_finished() {
                    remove.push(id.clone());
                }
            }

            for r in remove {
                let watched = inner.watching.remove(&r).unwrap();
                let ret = watched
                    .thread
                    .join()
                    .expect("Download thread shouldn't panic");
                let message = match ret {
                    Ok(_) => {
                        format!(
                            "Downloaded {:?} - {}",
                            watched.info.title, watched.info.uploader
                        )
                    }
                    Err(e) => {
                        format!(
                            "Failed to download {:?} - {}: {e}",
                            watched.info.title, watched.info.uploader
                        )
                    }
                };
                if !silent {
                    let pb = self.progress_bars.remove(&r).unwrap();
                    pb.finish_with_message(message);
                }
                inner.downloaded.insert(r, watched.info);
            }

            for id in inner.ids.clone() {
                match &id {
                    Id::Yt { yt_id } => {
                        let Ok(Some(info)) = live_info(&yt_dlp, &yt_id) else {
                            continue;
                        };
                        let video_id = Id::Yt {
                            yt_id: info.id.clone(),
                        };
                        if !(info.is_live || info.was_live)
                            || inner.watching.contains_key(&video_id)
                            || inner.downloaded.contains_key(&video_id)
                        {
                            continue;
                        }
                        let Ok(watching) = Watching::watch(yt_dlp.clone(), &video_id)
                        else {
                            continue;
                        };
                        inner.watching.insert(video_id.clone(), watching);
                        if !silent {
                            let pb = pbar();
                            pb.set_message(video_id.to_string());
                            let pb = self.multi_progress.add(pb);
                            self.progress_bars.insert(video_id.clone(), pb);
                        }
                    }
                    Id::Twitch { twitch_id }
                        if !inner.watching.contains_key(&id)
                            && !inner.downloaded.contains_key(&id)
                            && twitch_is_live(&yt_dlp, &twitch_id) =>
                    {
                        let Ok(watching) = Watching::watch(yt_dlp.clone(), &id) else {
                            continue;
                        };
                        inner.watching.insert(id.clone(), watching);
                        if !silent {
                            let pb = pbar();
                            pb.set_message(id.to_string());
                            let pb = self.multi_progress.add(pb);
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
    GetDownloaded,
}

#[derive(Debug, Deserialize, Serialize)]
enum IpcResponse {
    Watching(Vec<Info>),
    Downloaded(Vec<Info>),
    Error(String),
}

struct Ipc {
    inner_sub: Arc<Mutex<InnerSub>>,
    listener:  UnixListener,
}

impl Ipc {
    fn new(inner_sub: Arc<Mutex<InnerSub>>) -> eyre::Result<Self> {
        let runtime_dir = dirs::runtime_dir().expect("User runtime dir").join(NAME);
        let socket = runtime_dir.join("ipc.sock");

        if !runtime_dir.exists() {
            fs::create_dir_all(&runtime_dir)?;
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
                let watching = inner
                    .watching
                    .values()
                    .map(|x| x.info.clone())
                    .collect::<Vec<_>>();

                IpcResponse::Watching(watching)
            }
            IpcRequest::GetDownloaded => {
                let inner = self.inner_sub.lock().unwrap();
                let x = inner.downloaded.values().cloned().collect::<Vec<_>>();

                IpcResponse::Downloaded(x)
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
    /// Find out what streams the server has downloaded.
    GetDownloaded,
}

fn main() -> eyre::Result<()> {
    let command = Args::parse();
    match command {
        Args::Watch { silent } => serve(silent),
        Args::Ipc { subcommand } => ipc(subcommand),
        Args::Completions => {
            use clap::CommandFactory;
            use clap_complete::Shell;
            let mut cmd = Args::command();
            let shell = Shell::from_env()
                .ok_or_else(|| eyre!("Couldn't determine shell from environment!"))?;

            match shell {
                Shell::Fish => {
                    let vendor_completions_dir = dirs::data_dir()
                        .expect("data dir")
                        .join("fish/vendor_completions.d");
                    let vendor_completions_path = vendor_completions_dir.join("vdl.fish");
                    if !vendor_completions_dir.exists() {
                        fs::create_dir_all(&vendor_completions_dir)?;
                    }
                    let mut f = File::create(&vendor_completions_path)?;
                    eprintln!("Writing completions to {vendor_completions_path:?}");
                    clap_complete::generate(shell, &mut cmd, NAME, &mut f);
                }
                _ => {
                    clap_complete::generate(
                        shell,
                        &mut cmd,
                        NAME,
                        &mut std::io::stdout(),
                    );
                }
            };
            Ok(())
        }
    }
}

fn ipc(command: IpcCommand) -> eyre::Result<()> {
    let runtime_dir = dirs::runtime_dir().expect("User runtime dir").join(NAME);
    let socket = runtime_dir.join("ipc.sock");
    let mut stream = UnixStream::connect(&socket).map_err(|e| {
        eyre!(
            "Couldn't connect to socket {socket:?} (ensure an instance is running): {e}"
        )
    })?;

    let request = match command {
        IpcCommand::GetWatching => IpcRequest::GetWatching,
        IpcCommand::GetDownloaded => IpcRequest::GetDownloaded,
    };
    let request_json = serde_json::ser::to_vec(&request)?;
    stream.write(&request_json)?;
    stream.shutdown(Shutdown::Write)?;

    let mut response_json = Vec::new();
    stream.read_to_end(&mut response_json)?;

    let response: IpcResponse = serde_json::de::from_slice(&response_json)?;
    match response {
        IpcResponse::Watching(watching) => {
            // TODO: Tabular display
            println!("Watching {} streams", watching.len());
            if !watching.is_empty() {
                for info in watching {
                    println!(
                        ":: {:?} - {} ({:?})",
                        info.title, info.uploader, info.webpage_url
                    );
                }
            }
        }
        IpcResponse::Downloaded(downloaded) => {
            // TODO: Tabular display
            println!("Downloaded {} streams", downloaded.len());
            if !downloaded.is_empty() {
                for info in downloaded {
                    println!(
                        ":: {:?} - {} ({:?})",
                        info.title, info.uploader, info.webpage_url,
                    );
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

    YtDlp::download_latest()?;

    let subscriber = std::thread::spawn(move || subscriber.spawn(silent));

    loop {
        if subscriber.is_finished() {
            eprintln!("Subscriber exited!");
            subscriber.join().unwrap()?;
            std::process::exit(1);
        }
        if exit.swap(false, Ordering::Relaxed) {
            let runtime_dir = dirs::runtime_dir().expect("User runtime dir").join(NAME);
            let socket = runtime_dir.join("ipc.sock");
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
                    eprintln!("Failed to reload config (retaining previous config): {e}")
                }
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

fn live_info(yt_dlp: &YtDlp, id: &str) -> eyre::Result<Option<YtLiveInfo>> {
    let url = format!("https://www.youtube.com/{id}/live");
    let output = yt_dlp.command_with_args().arg("-J").arg(url).output()?;
    let stdout = String::from_utf8(output.stdout)?;
    let stdout = stdout.trim();
    if stdout.is_empty() {
        return Ok(None);
    }
    let info: YtLiveInfo = serde_json::from_str(&stdout)?;
    Ok(Some(info))
}

fn dl(yt_dlp: &YtDlp, url: &str, dl_dir: PathBuf) -> eyre::Result<()> {
    let current_dir = std::env::current_dir()?;
    let Ok(output) = yt_dlp
        .command_with_args()
        .args([&url, "--print", "_filename"])
        .output()
    else {
        return Err(eyre!("Couldn't get filename"));
    };
    let stdout = String::from_utf8(output.stdout)?;
    let stdout = stdout.trim();
    if stdout.is_empty() {
        return Err(eyre!("Filename is empty"));
    }
    let mut output_filename = PathBuf::from(stdout);
    if let Some(format) = yt_dlp.remux_video.as_deref() {
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

    let mut oo = OpenOptions::new();
    oo.create(true).append(true);
    let stdout = match oo.open(&stdout_path) {
        Ok(x) => Stdio::from(x),
        Err(_) => Stdio::null(),
    };
    let stderr = match oo.open(&stderr_path) {
        Ok(x) => Stdio::from(x),
        Err(_) => Stdio::null(),
    };

    let _status = yt_dlp
        .command_with_args()
        .current_dir(&dl_dir)
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

fn yt_dl(yt_dlp: &YtDlp, id: &str, dl_dir: PathBuf) -> eyre::Result<()> {
    let url = format!("https://www.youtube.com/watch?v={id}");
    dl(yt_dlp, &url, dl_dir)
}

fn twitch_is_live(yt_dlp: &YtDlp, id: &str) -> bool {
    let url = format!("https://www.twitch.tv/{id}");
    let Ok(output) = yt_dlp
        .command_with_args()
        .args([&url, "--quiet", "--simulate"])
        .output()
    else {
        return false;
    };

    String::from_utf8(output.stderr)
        .is_ok_and(|x| !x.contains("The channel is not currently live"))
}

fn twitch_dl(yt_dlp: &YtDlp, id: &str, dl_dir: PathBuf) -> eyre::Result<()> {
    let url = format!("https://www.twitch.tv/{id}");
    dl(yt_dlp, &url, dl_dir)
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Info {
    id:          String,
    title:       String,
    uploader:    String,
    webpage_url: String,
}

impl Info {
    fn get(yt_dlp: &YtDlp, id: &Id) -> eyre::Result<Self> {
        let url = match id {
            Id::Yt { yt_id } => format!("https://www.youtube.com/watch?v={yt_id}"),
            Id::Twitch { twitch_id } => format!("https://www.twitch.tv/{twitch_id}"),
        };
        let output = yt_dlp.command_with_args().args(["-J", &url]).output()?;
        let stdout = String::from_utf8(output.stdout)?;
        Ok(serde_json::from_str(&stdout)?)
    }
}

// TODO:
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
enum Id {
    Yt { yt_id: String },
    Twitch { twitch_id: String },
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
