use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use indicatif::{ProgressBar, ProgressStyle};

pub struct CliSpinner {
    progress_bar: ProgressBar,
}

impl CliSpinner {
    pub fn new(message: impl Into<String>) -> Self {
        let progress_bar = ProgressBar::new_spinner();
        progress_bar.set_style(
            ProgressStyle::with_template("{spinner:.green} {msg}")
                .expect("spinner template must be valid"),
        );
        progress_bar.enable_steady_tick(Duration::from_millis(80));
        progress_bar.set_message(message.into());
        progress_bar.tick();
        Self { progress_bar }
    }

    pub fn finish_success(self, message: impl Into<String>) {
        self.progress_bar.finish_and_clear();
        print_success(message.into());
    }

    pub fn set_message(&self, message: impl Into<String>) {
        self.progress_bar.set_message(message.into());
        self.progress_bar.tick();
    }

    pub fn finish_clear(self) {
        self.progress_bar.finish_and_clear();
    }

    pub fn suspend<T>(&self, callback: impl FnOnce() -> T) -> T {
        self.progress_bar.suspend(callback)
    }

    pub fn finish_warning(self, message: impl Into<String>) {
        self.progress_bar.finish_and_clear();
        println!("{} {}", yellow_symbol("⚠"), message.into());
    }

    pub fn finish_failure(self, message: impl Into<String>) {
        self.progress_bar.finish_and_clear();
        println!("{} {}", red_symbol("✖"), message.into());
    }
}

pub struct CliDownloadProgress {
    label: String,
    total_bytes: Option<u64>,
    started_at: Instant,
    last_draw_at: Instant,
    last_render_len: usize,
    last_logged_percent: u64,
    interactive: bool,
}

impl CliDownloadProgress {
    pub fn new(label: impl Into<String>, total_bytes: Option<u64>) -> Self {
        let label = label.into();
        let interactive = io::stderr().is_terminal();
        let progress = Self {
            label,
            total_bytes,
            started_at: Instant::now(),
            last_draw_at: Instant::now() - Duration::from_secs(1),
            last_render_len: 0,
            last_logged_percent: 0,
            interactive,
        };

        if !progress.interactive {
            match progress.total_bytes {
                Some(total_bytes) => eprintln!(
                    "orbi > download {} ({})",
                    progress.label,
                    human_bytes(total_bytes)
                ),
                None => eprintln!("orbi > download {}", progress.label),
            }
        }

        progress
    }

    pub fn advance(&mut self, downloaded_bytes: u64) {
        if self.interactive {
            if self.last_draw_at.elapsed() >= Duration::from_millis(100) {
                self.draw(downloaded_bytes);
            }
            return;
        }

        let Some(total_bytes) = self.total_bytes else {
            return;
        };
        if total_bytes == 0 {
            return;
        }

        let percent = (downloaded_bytes.saturating_mul(100) / total_bytes).min(100);
        if percent >= self.last_logged_percent + 10 {
            self.last_logged_percent = percent;
            eprintln!(
                "orbi > download {} {} of {} ({}%)",
                self.label,
                human_bytes(downloaded_bytes),
                human_bytes(total_bytes),
                percent
            );
        }
    }

    pub fn finish(&mut self, downloaded_bytes: u64, destination: &Path) {
        if self.interactive {
            self.clear_render();
        }

        print_success(format!(
            "Downloaded {} to {} ({}, {})",
            self.label,
            destination.display(),
            human_bytes(downloaded_bytes),
            human_duration(self.started_at.elapsed())
        ));
    }

    fn draw(&mut self, downloaded_bytes: u64) {
        let rate = if self.started_at.elapsed().as_secs_f64() > 0.0 {
            downloaded_bytes as f64 / self.started_at.elapsed().as_secs_f64()
        } else {
            0.0
        };

        let detail = match self.total_bytes {
            Some(total_bytes) if total_bytes > 0 => {
                let percent = (downloaded_bytes.saturating_mul(100) / total_bytes).min(100);
                format!(
                    "{} {} / {} ({}%) {}/s",
                    progress_bar(downloaded_bytes, total_bytes, 24),
                    human_bytes(downloaded_bytes),
                    human_bytes(total_bytes),
                    percent,
                    human_bytes(rate as u64)
                )
            }
            _ => format!(
                "{} downloaded {}/s",
                human_bytes(downloaded_bytes),
                human_bytes(rate as u64)
            ),
        };

        let line = format!("orbi > download {} {}", self.label, detail);
        let padding = " ".repeat(self.last_render_len.saturating_sub(line.len()));
        eprint!("\r{line}{padding}");
        let _ = io::stderr().flush();
        self.last_render_len = line.len();
        self.last_draw_at = Instant::now();
    }

    fn clear_render(&mut self) {
        if self.last_render_len == 0 {
            return;
        }

        eprint!("\r{}\r", " ".repeat(self.last_render_len));
        let _ = io::stderr().flush();
        self.last_render_len = 0;
    }
}

pub fn print_success(message: impl AsRef<str>) {
    println!("{} {}", green_symbol("✔"), message.as_ref());
}

pub fn human_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;

    let value = bytes as f64;
    if value >= GIB {
        format!("{:.1} GiB", value / GIB)
    } else if value >= MIB {
        format!("{:.1} MiB", value / MIB)
    } else if value >= KIB {
        format!("{:.1} KiB", value / KIB)
    } else {
        format!("{bytes} B")
    }
}

pub fn format_elapsed(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let minutes = seconds / 60;
    let remaining_seconds = seconds % 60;
    if minutes == 0 {
        format!("{remaining_seconds}s")
    } else {
        format!("{minutes}m {remaining_seconds:02}s")
    }
}

fn progress_bar(downloaded_bytes: u64, total_bytes: u64, width: usize) -> String {
    if total_bytes == 0 || width == 0 {
        return "[]".to_owned();
    }

    let filled =
        ((downloaded_bytes.saturating_mul(width as u64)) / total_bytes).min(width as u64) as usize;
    format!(
        "[{}{}]",
        "#".repeat(filled),
        "-".repeat(width.saturating_sub(filled))
    )
}

fn human_duration(duration: Duration) -> String {
    if duration.as_secs() >= 60 {
        format!(
            "{}m{:02}s",
            duration.as_secs() / 60,
            duration.as_secs() % 60
        )
    } else if duration.as_secs() > 0 {
        format!("{}s", duration.as_secs())
    } else {
        format!("{}ms", duration.as_millis())
    }
}

fn green_symbol(symbol: &str) -> String {
    format!("\x1b[32m{symbol}\x1b[0m")
}

fn yellow_symbol(symbol: &str) -> String {
    format!("\x1b[33m{symbol}\x1b[0m")
}

fn red_symbol(symbol: &str) -> String {
    format!("\x1b[31m{symbol}\x1b[0m")
}

#[cfg(test)]
mod tests {
    use super::{human_bytes, progress_bar};

    #[test]
    fn formats_binary_sizes() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1024 * 1024), "1.0 MiB");
    }

    #[test]
    fn renders_ascii_progress_bars() {
        assert_eq!(progress_bar(50, 100, 10), "[#####-----]");
    }
}
