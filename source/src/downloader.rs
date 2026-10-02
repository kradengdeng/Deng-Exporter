use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify, Semaphore, mpsc};
use tokio::process::Command;
use tokio::fs;
use crate::{AppState, DownloadItem, DownloadStatus, Mode};
use std::process::Stdio;
use std::path::Path;

const FAST_AUDIO_SLOTS: usize = 30;
const FAST_VIDEO_SLOTS: usize = 15;

struct DownloadJob {
    index: usize,
    url: String,
    mode: Mode,
    output_dir: std::path::PathBuf,
}

pub struct DownloadManager {
    state: Arc<Mutex<AppState>>,
    ui_tx: mpsc::Sender<()>,

    /// Pending jobs not yet started (queue mode only).
    /// Shared with the sequential worker so we can drain it on fast-mode toggle.
    pending: Arc<Mutex<VecDeque<DownloadJob>>>,
    worker_notify: Arc<Notify>,
    worker_handle: tokio::task::JoinHandle<()>,

    /// Semaphores that cap fast-mode concurrency.
    audio_sem: Arc<Semaphore>,
    video_sem: Arc<Semaphore>,
    /// Handles of in-flight fast-mode tasks (for cancel).
    fast_tasks: Vec<tokio::task::JoinHandle<()>>,
}

fn yt_dlp_cmd() -> Command {
    if Path::new("yt-dlp.exe").exists() {
        Command::new(".\\yt-dlp.exe")
    } else {
        Command::new("yt-dlp")
    }
}

impl DownloadManager {
    pub fn new(state: Arc<Mutex<AppState>>, ui_tx: mpsc::Sender<()>) -> Self {
        let pending = Arc::new(Mutex::new(VecDeque::<DownloadJob>::new()));
        let notify  = Arc::new(Notify::new());

        let worker_state  = state.clone();
        let worker_ui_tx  = ui_tx.clone();
        let worker_pending = pending.clone();
        let worker_notify  = notify.clone();
        let worker_handle  = tokio::spawn(run_queue_worker(
            worker_pending, worker_notify, worker_state, worker_ui_tx,
        ));

        Self {
            state,
            ui_tx,
            pending,
            worker_notify: notify,
            worker_handle,
            audio_sem: Arc::new(Semaphore::new(FAST_AUDIO_SLOTS)),
            video_sem: Arc::new(Semaphore::new(FAST_VIDEO_SLOTS)),
            fast_tasks: Vec::new(),
        }
    }

    // ── Public API ──────────────────────────────────────────────────────────

    pub async fn submit(&mut self, input: String) {
        let input_clean = input.trim_matches('"');
        if input_clean.ends_with(".txt") {
            if let Ok(contents) = fs::read_to_string(input_clean).await {
                for line in contents.lines() {
                    let url = line.trim();
                    if !url.is_empty() {
                        self.enqueue(url.to_string()).await;
                    }
                }
                return;
            }
        }
        self.enqueue(input_clean.to_string()).await;
    }

    /// Called by main when the user presses F6 to toggle fast mode.
    /// If switching TO fast mode, drains all pending Queued jobs and spawns
    /// them in parallel. The currently active (Loading/Downloading) sequential
    /// job is left alone and finishes normally.
    pub async fn on_fast_mode_toggled(&mut self, fast: bool) {
        if fast {
            // Drain every job that is still waiting in the shared deque.
            let drained: Vec<DownloadJob> = {
                let mut q = self.pending.lock().await;
                q.drain(..).collect()
            };
            for job in drained {
                self.spawn_fast_job(job);
            }
        }
        // Switching back to queue mode: nothing to do — fast tasks keep running
        // and new submissions will go to the sequential worker.
    }

    pub async fn cancel_all(&mut self) {
        // Stop the sequential worker and clear pending queue.
        self.worker_handle.abort();
        { self.pending.lock().await.clear(); }

        // Abort all in-flight fast tasks.
        for t in self.fast_tasks.drain(..) { t.abort(); }

        {
            let mut state = self.state.lock().await;
            let mut changed = false;
            for item in state.downloads.iter_mut() {
                if matches!(
                    item.status,
                    DownloadStatus::Queued | DownloadStatus::Loading | DownloadStatus::Downloading(_)
                ) {
                    item.status = DownloadStatus::Cancelled;
                    changed = true;
                }
            }
            if changed { state.is_dirty = true; }
        }
        let _ = self.ui_tx.send(()).await;

        // Restart a fresh worker for future queue-mode submissions.
        let (pending, notify) = self.restart_worker();
        self.pending = pending;
        self.worker_notify = notify;
    }

    // ── Private helpers ─────────────────────────────────────────────────────

    async fn enqueue(&mut self, url: String) {
        let fast_mode = { self.state.lock().await.fast_mode };

        let (index, mode, output_dir) = {
            let mut state = self.state.lock().await;
            let index = state.downloads.len();
            let mode  = state.mode;
            let output_dir = state.output_path.clone();

            state.downloads.push(DownloadItem {
                url: url.clone(),
                title: None,
                status: if fast_mode { DownloadStatus::Loading } else { DownloadStatus::Queued },
            });
            state.is_dirty = true;

            let (_, rows) = crossterm::terminal::size().unwrap_or((80, 24));
            let avail = rows.saturating_sub(7) as usize;
            let max_scroll = (index + 1).saturating_sub(avail);
            if state.scroll_offset < max_scroll { state.scroll_offset = max_scroll; }

            (index, mode, output_dir)
        };
        let _ = self.ui_tx.send(()).await;

        let job = DownloadJob { index, url, mode, output_dir };

        if fast_mode {
            self.spawn_fast_job(job);
        } else {
            self.pending.lock().await.push_back(job);
            self.worker_notify.notify_one();
        }
    }

    fn spawn_fast_job(&mut self, job: DownloadJob) {
        let sem = match job.mode {
            Mode::Audio => self.audio_sem.clone(),
            Mode::Video => self.video_sem.clone(),
        };
        let state_clone  = self.state.clone();
        let ui_tx_clone  = self.ui_tx.clone();
        let handle = tokio::spawn(async move {
            let _permit = sem.acquire().await;
            process_job(job, &state_clone, &ui_tx_clone).await;
        });
        self.fast_tasks.push(handle);
    }

    fn restart_worker(&mut self) -> (Arc<Mutex<VecDeque<DownloadJob>>>, Arc<Notify>) {
        let pending = Arc::new(Mutex::new(VecDeque::<DownloadJob>::new()));
        let notify  = Arc::new(Notify::new());
        self.worker_handle = tokio::spawn(run_queue_worker(
            pending.clone(), notify.clone(),
            self.state.clone(), self.ui_tx.clone(),
        ));
        (pending, notify)
    }
}

// ── Sequential worker ────────────────────────────────────────────────────────

async fn run_queue_worker(
    pending: Arc<Mutex<VecDeque<DownloadJob>>>,
    notify: Arc<Notify>,
    state: Arc<Mutex<AppState>>,
    ui_tx: mpsc::Sender<()>,
) {
    loop {
        let job = { pending.lock().await.pop_front() };
        if let Some(job) = job {
            process_job(job, &state, &ui_tx).await;
            // Immediately loop to check for more without waiting.
        } else {
            // Queue is empty — sleep until notified.
            notify.notified().await;
        }
    }
}

// ── Core download logic (shared between queue and fast paths) ─────────────────

async fn process_job(
    job: DownloadJob,
    state: &Arc<Mutex<AppState>>,
    ui_tx: &mpsc::Sender<()>,
) {
    let DownloadJob { index, url, mode, output_dir } = job;

    // Phase 1: mark Loading & fetch title
    {
        let mut s = state.lock().await;
        if let Some(item) = s.downloads.get_mut(index) {
            if matches!(item.status, DownloadStatus::Cancelled) { return; }
            item.status = DownloadStatus::Loading;
            s.is_dirty = true;
        }
    }
    let _ = ui_tx.send(()).await;

    let mut cmd = yt_dlp_cmd();
    cmd.arg("--print").arg("title").arg(&url)
        .stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);

    let title = if let Ok(output) = cmd.output().await {
        if output.status.success() {
            let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if s.is_empty() { "Unknown Title".to_string() } else { s }
        } else { "Unknown Title".to_string() }
    } else { "Unknown Title".to_string() };

    {
        let s = state.lock().await;
        if s.downloads.get(index).map_or(false, |i| matches!(i.status, DownloadStatus::Cancelled)) {
            return;
        }
    }

    {
        let mut s = state.lock().await;
        if let Some(item) = s.downloads.get_mut(index) {
            item.title = Some(title.clone());
            item.status = DownloadStatus::Downloading(0.0);
            s.is_dirty = true;
        }
    }
    let _ = ui_tx.send(()).await;

    // Phase 2: actual download
    let out_tmpl = output_dir.join("%(title)s.%(ext)s");
    let mut dl_cmd = yt_dlp_cmd();
    if mode == Mode::Audio {
        dl_cmd.arg("-x").arg("--audio-format").arg("mp3")
              .arg("-o").arg(out_tmpl.to_string_lossy().as_ref())
              .arg("--newline").arg(&url);
    } else {
        dl_cmd.arg("-f").arg("bestvideo[ext=mp4]+bestaudio[ext=m4a]/best[ext=mp4]/best")
              .arg("-o").arg(out_tmpl.to_string_lossy().as_ref())
              .arg("--newline").arg(&url);
    }
    if Path::new("ffmpeg.exe").exists() {
        dl_cmd.arg("--ffmpeg-location").arg(".\\ffmpeg.exe");
    }
    dl_cmd.stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);

    let final_status = if let Ok(mut child) = dl_cmd.spawn() {
        if let Some(stdout) = child.stdout.take() {
            use tokio::io::{BufReader, AsyncBufReadExt};
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            while let Ok(bytes) = reader.read_line(&mut line).await {
                if bytes == 0 { break; }
                if line.contains("[download]") && line.contains('%') {
                    if let Some(start) = line.find("[download]") {
                        let part = &line[start + 10..];
                        if let Some(end) = part.find('%') {
                            if let Ok(p) = part[..end].trim().parse::<f32>() {
                                let mut s = state.lock().await;
                                if let Some(item) = s.downloads.get_mut(index) {
                                    if !matches!(item.status, DownloadStatus::Cancelled) {
                                        item.status = DownloadStatus::Downloading(p);
                                        s.is_dirty = true;
                                    }
                                }
                                let _ = ui_tx.send(()).await;
                            }
                        }
                    }
                }
                line.clear();
            }
        }
        if let Ok(exit) = child.wait().await {
            if exit.success() {
                DownloadStatus::Completed
            } else {
                let s = state.lock().await;
                if s.downloads.get(index).map_or(false, |i| matches!(i.status, DownloadStatus::Cancelled)) {
                    DownloadStatus::Cancelled
                } else {
                    DownloadStatus::Error("Failed".to_string())
                }
            }
        } else { DownloadStatus::Error("Failed to wait".to_string()) }
    } else { DownloadStatus::Error("Failed to start yt-dlp".to_string()) };

    {
        let mut s = state.lock().await;
        if let Some(item) = s.downloads.get_mut(index) {
            if !matches!(item.status, DownloadStatus::Cancelled) {
                item.status = final_status;
                s.is_dirty = true;
            }
        }
    }
    let _ = ui_tx.send(()).await;
}
