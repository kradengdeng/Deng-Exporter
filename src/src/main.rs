mod ui;
mod downloader;
mod installer;

use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers, KeyboardEnhancementFlags, PushKeyboardEnhancementFlags, PopKeyboardEnhancementFlags},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen, SetTitle},
    ExecutableCommand,
};
use std::{
    io::{self, stdout},
    time::Duration,
};
use tokio::sync::mpsc;
use std::sync::Arc;
use tokio::sync::Mutex;
use crate::downloader::DownloadManager;

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Audio,
    Video,
}

#[derive(Clone, PartialEq)]
pub enum AppScreen {
    Installing,
    Main,
}

#[derive(Clone)]
pub struct DownloadItem {
    pub url: String,
    pub title: Option<String>,
    pub status: DownloadStatus,
}

#[derive(Clone, PartialEq)]
pub enum DownloadStatus {
    Queued,
    Loading,
    Downloading(f32),
    Completed,
    Error(String),
    Cancelled,
}

pub struct AppState {
    pub screen: AppScreen,
    pub install_progress: f32,
    pub mode: Mode,
    pub input_buffer: String,
    pub downloads: Vec<DownloadItem>,
    pub is_dirty: bool,
    pub scroll_offset: usize,
    pub output_path: std::path::PathBuf,
    /// When true downloads run in parallel (up to 30 audio / 15 video).
    pub fast_mode: bool,
}

#[tokio::main]
async fn main() -> io::Result<()> {
    enable_raw_mode()?;
    let mut out = stdout();
    out.execute(EnterAlternateScreen)?;
    out.execute(SetTitle("Deng : Exporter"))?;
    // Enable keyboard enhancement so we can detect modifier keys (Alt, etc.) as standalone events.
    let _ = out.execute(PushKeyboardEnhancementFlags(
        KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
    ));
    
    let orig_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = stdout().execute(LeaveAlternateScreen);
        let _ = disable_raw_mode();
        orig_hook(panic_info);
    }));

    let download_dir = std::env::var("USERPROFILE").map(|p| format!("{}\\Downloads", p)).unwrap_or_else(|_| ".".to_string());

    let state = Arc::new(Mutex::new(AppState {
        screen: AppScreen::Installing,
        install_progress: 0.0,
        mode: Mode::Audio,
        input_buffer: String::new(),
        downloads: Vec::new(),
        is_dirty: true,
        scroll_offset: 0,
        output_path: std::path::PathBuf::from(download_dir),
        fast_mode: false,
    }));

    let (tx, mut rx) = mpsc::channel(100);
    let mut manager = DownloadManager::new(state.clone(), tx.clone());

    tokio::spawn(installer::check_and_install(state.clone(), tx.clone()));

    loop {
        let mut should_draw = false;
        {
            let mut state_guard = state.lock().await;
            if state_guard.is_dirty {
                should_draw = true;
                state_guard.is_dirty = false;
            }
        }
        
        if should_draw {
            let state_guard = state.lock().await;
            ui::draw(&state_guard)?;
        }

        while let Ok(_) = rx.try_recv() {}

        if event::poll(Duration::from_millis(30))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == event::KeyEventKind::Release {
                    continue;
                }

                if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('c') {
                    manager.cancel_all().await;
                    break;
                }
                
                let is_installing = {
                    let s = state.lock().await;
                    s.screen == AppScreen::Installing
                };
                
                if is_installing {
                    if key.code == KeyCode::Esc {
                        break;
                    }
                    continue;
                }

                match key.code {
                    KeyCode::Esc => {
                        manager.cancel_all().await;
                        break;
                    }
                    KeyCode::F(5) => {
                        let mut s = state.lock().await;
                        s.mode = if s.mode == Mode::Audio { Mode::Video } else { Mode::Audio };
                        s.is_dirty = true;
                    }
                    KeyCode::F(6) => {
                        let new_fast = {
                            let mut s = state.lock().await;
                            s.fast_mode = !s.fast_mode;
                            s.is_dirty = true;
                            s.fast_mode
                        };
                        manager.on_fast_mode_toggled(new_fast).await;
                    }
                    KeyCode::Up => {
                        let mut s = state.lock().await;
                        if s.scroll_offset > 0 {
                            s.scroll_offset -= 1;
                            s.is_dirty = true;
                        }
                    }
                    KeyCode::Down => {
                        let mut s = state.lock().await;
                        let (_, rows) = crossterm::terminal::size().unwrap_or((80, 24));
                        let available = (rows.saturating_sub(7)) as usize;
                        if s.scroll_offset + available < s.downloads.len() {
                            s.scroll_offset += 1;
                            s.is_dirty = true;
                        }
                    }
                    KeyCode::Tab => {
                        let tx_clone = tx.clone();
                        let state_clone = state.clone();
                        tokio::spawn(async move {
                            // -STA is required for AutoUpgradeEnabled to show the modern Windows Explorer picker.
                            let ps_script = r#"Add-Type -AssemblyName System.windows.forms; $f = New-Object System.Windows.Forms.FolderBrowserDialog; $f.AutoUpgradeEnabled = $true; $f.ShowNewFolderButton = $true; if ($f.ShowDialog() -eq 'OK') { Write-Output $f.SelectedPath }"#;
                            let output = tokio::process::Command::new("powershell")
                                .args(&["-NoProfile", "-STA", "-Command", ps_script])
                                .output().await;
                            if let Ok(out) = output {
                                let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
                                if !path.is_empty() {
                                    let mut s = state_clone.lock().await;
                                    s.output_path = std::path::PathBuf::from(path);
                                    s.is_dirty = true;
                                    let _ = tx_clone.send(()).await;
                                }
                            }
                        });
                    }

                    KeyCode::Char(' ') => {
                        let has_active = {
                            let state_guard = state.lock().await;
                            state_guard.downloads.iter().any(|d| matches!(d.status, DownloadStatus::Loading | DownloadStatus::Downloading(_)))
                        };
                        if has_active {
                            manager.cancel_all().await;
                        } else {
                            let mut state_guard = state.lock().await;
                            state_guard.input_buffer.push(' ');
                            state_guard.is_dirty = true;
                        }
                    }
                    KeyCode::Char(c) => {
                        let mut state_guard = state.lock().await;
                        state_guard.input_buffer.push(c);
                        state_guard.is_dirty = true;
                    }
                    KeyCode::Backspace => {
                        let mut state_guard = state.lock().await;
                        if state_guard.input_buffer.pop().is_some() {
                            state_guard.is_dirty = true;
                        }
                    }
                    KeyCode::Enter => {
                        let input = {
                            let mut state_guard = state.lock().await;
                            let val = state_guard.input_buffer.clone();
                            state_guard.input_buffer.clear();
                            state_guard.is_dirty = true;
                            val
                        };
                        let input_trimmed = input.trim();
                        if !input_trimmed.is_empty() {
                            manager.submit(input_trimmed.to_string()).await;
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    let _ = stdout().execute(PopKeyboardEnhancementFlags);
    stdout().execute(LeaveAlternateScreen)?;
    disable_raw_mode()?;
    Ok(())
}
