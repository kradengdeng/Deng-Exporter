use crossterm::{
    cursor::{Hide, MoveTo, Show},
    style::{Color, Print, ResetColor, SetForegroundColor},
    terminal::{Clear, ClearType},
    ExecutableCommand,
};
use std::io::{self, stdout, Write};
use crate::{AppState, AppScreen, Mode, DownloadStatus};

pub fn draw(state: &AppState) -> io::Result<()> {
    let mut out = stdout();
    out.execute(Hide)?;
    
    let (_, rows) = crossterm::terminal::size()?;

    if state.screen == AppScreen::Installing {
        out.execute(MoveTo(0, 0))?;
        out.execute(SetForegroundColor(Color::White))?;
        out.execute(Print(format!("Installing requirements... ({:.1}% Completed)", state.install_progress)))?;
        out.execute(Clear(ClearType::UntilNewLine))?;
        out.execute(Print("\n"))?;
        out.execute(ResetColor)?;
        
        for r in 1..rows {
            out.execute(MoveTo(0, r))?;
            out.execute(Clear(ClearType::CurrentLine))?;
        }
        
        out.execute(Show)?;
        out.flush()?;
        return Ok(());
    }

    let light_purple = Color::Rgb { r: 216, g: 134, b: 255 };

    // Header (Row 0)
    out.execute(MoveTo(0, 0))?;
    out.execute(SetForegroundColor(Color::White))?;
    out.execute(Print("Select: "))?;
    let mode_str = match state.mode {
        Mode::Audio => "MP3 (Audio)",
        Mode::Video => "MP4 (Video)",
    };
    if state.mode == Mode::Audio {
        out.execute(SetForegroundColor(light_purple))?;
    } else {
        out.execute(SetForegroundColor(Color::Cyan))?;
    }
    out.execute(Print(mode_str))?;
    // Fast mode indicator
    out.execute(SetForegroundColor(Color::White))?;
    out.execute(Print("   Fast: "))?;
    if state.fast_mode {
        out.execute(SetForegroundColor(Color::Yellow))?;
        out.execute(Print("ON "))?;
        out.execute(SetForegroundColor(Color::DarkGrey))?;
        let limit = match state.mode {
            Mode::Audio => "(30 parallel)",
            Mode::Video => "(15 parallel)",
        };
        out.execute(Print(limit))?;
    } else {
        out.execute(SetForegroundColor(Color::DarkGrey))?;
        out.execute(Print("OFF (queue)"))?;
    }
    out.execute(Clear(ClearType::UntilNewLine))?;
    
    // Row 1
    out.execute(MoveTo(0, 1))?;
    out.execute(ResetColor)?;
    out.execute(Print("Please enter Youtube URL to start download "))?;
    out.execute(Print(match state.mode {
        Mode::Audio => "audio ",
        Mode::Video => "video ",
    }))?;
    out.execute(Print("or enter file path (.txt) with mutiple link to"))?;
    out.execute(Clear(ClearType::UntilNewLine))?;
    
    // Row 2
    out.execute(MoveTo(0, 2))?;
    let row2_text = if state.fast_mode {
        match state.mode {
            Mode::Audio => "download mutiple audio at once (30 Max)".to_string(),
            Mode::Video => "download mutiple video at once (15 Max)".to_string(),
        }
    } else {
        match state.mode {
            Mode::Audio => "download audio one by one (Queue mode)".to_string(),
            Mode::Video => "download video one by one (Queue mode)".to_string(),
        }
    };
    out.execute(Print(row2_text))?;
    out.execute(Clear(ClearType::UntilNewLine))?;

    // Row 3: Output path
    out.execute(MoveTo(0, 3))?;
    out.execute(SetForegroundColor(Color::DarkGrey))?;
    out.execute(Print(format!("Output Path: {}", state.output_path.display())))?;
    out.execute(ResetColor)?;
    out.execute(Clear(ClearType::UntilNewLine))?;
    
    // Row 4: Empty Space
    out.execute(MoveTo(0, 4))?;
    out.execute(Clear(ClearType::CurrentLine))?;

    // Downloads
    let header_lines = 5; // Rows 0 to 4
    let footer_lines = 2; // Footer + Input
    let available_list_rows = rows.saturating_sub(header_lines + footer_lines) as usize;

    let total_downloads = state.downloads.len();
    let max_scroll = total_downloads.saturating_sub(available_list_rows);
    let display_offset = state.scroll_offset.min(max_scroll);
    
    let visible_downloads = state.downloads.iter().skip(display_offset).take(available_list_rows);

    let mut current_row = header_lines;
    for item in visible_downloads {
        out.execute(MoveTo(0, current_row))?;
        match item.status {
            DownloadStatus::Queued => {
                out.execute(SetForegroundColor(Color::DarkGrey))?;
                out.execute(Print("Queued        "))?;
                out.execute(ResetColor)?;
                out.execute(Print(item.url.as_str()))?;
            }
            DownloadStatus::Loading => {
                out.execute(Print("Loading..."))?;
            }
            DownloadStatus::Downloading(pct) => {
                let title = item.title.as_deref().unwrap_or("Unknown Title");
                out.execute(Print(format!("Downloading... ({:.1}%)  {}", pct, title)))?;
            }
            DownloadStatus::Completed => {
                let title = item.title.as_deref().unwrap_or("Unknown Title");
                out.execute(SetForegroundColor(Color::Green))?;
                out.execute(Print("COMPLETED!    "))?;
                out.execute(ResetColor)?;
                out.execute(Print(title))?;
            }
            DownloadStatus::Error(ref err) => {
                let title = item.title.as_deref().unwrap_or("Unknown Title");
                out.execute(SetForegroundColor(Color::Red))?;
                out.execute(Print("ERROR!        "))?;
                out.execute(ResetColor)?;
                out.execute(Print(format!("{} ({})", title, err)))?;
            }
            DownloadStatus::Cancelled => {
                let title = item.title.as_deref().unwrap_or("Unknown Title");
                out.execute(SetForegroundColor(Color::DarkGrey))?;
                out.execute(Print("CANCELLED     "))?;
                out.execute(ResetColor)?;
                out.execute(Print(title))?;
            }
        }
        out.execute(Clear(ClearType::UntilNewLine))?;
        current_row += 1;
    }
    out.execute(ResetColor)?;

    // Clear empty lines between downloads and footer
    let footer_row = rows.saturating_sub(2);
    if current_row < footer_row {
        for r in current_row..footer_row {
            out.execute(MoveTo(0, r))?;
            out.execute(Clear(ClearType::CurrentLine))?;
        }
    }

    // Footer (Row rows - 2)
    out.execute(MoveTo(0, footer_row))?;
    out.execute(SetForegroundColor(light_purple))?;
    out.execute(Print("F5"))?;
    out.execute(SetForegroundColor(Color::DarkGrey))?;
    out.execute(Print(" Mode   "))?;

    out.execute(SetForegroundColor(light_purple))?;
    out.execute(Print("F6"))?;
    out.execute(SetForegroundColor(Color::DarkGrey))?;
    out.execute(Print(" Fast DL   "))?;

    out.execute(SetForegroundColor(light_purple))?;
    out.execute(Print("Enter"))?;
    out.execute(SetForegroundColor(Color::DarkGrey))?;
    out.execute(Print(" Confirm   "))?;
    
    out.execute(SetForegroundColor(light_purple))?;
    out.execute(Print("Space"))?;
    out.execute(SetForegroundColor(Color::DarkGrey))?;
    out.execute(Print(" Cancel   "))?;

    out.execute(SetForegroundColor(light_purple))?;
    out.execute(Print("Tab"))?;
    out.execute(SetForegroundColor(Color::DarkGrey))?;
    out.execute(Print(" Set Path"))?;

    out.execute(Clear(ClearType::UntilNewLine))?;
    
    // Input (Row rows - 1)
    let input_row = rows.saturating_sub(1);
    out.execute(MoveTo(0, input_row))?;
    out.execute(ResetColor)?;
    out.execute(Print(format!(">> {}", state.input_buffer)))?;
    out.execute(Clear(ClearType::UntilNewLine))?;

    out.execute(Show)?;
    out.flush()?;
    Ok(())
}
