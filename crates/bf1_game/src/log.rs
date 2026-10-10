//! A small log file in logs/game-<unix time>.log. Lines also go to stdout.

use bevy::prelude::*;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Resource)]
pub struct GameLog {
    file: Mutex<std::fs::File>,
    pub path: PathBuf,
    start: std::time::Instant,
}

impl GameLog {
    pub fn create() -> std::io::Result<GameLog> {
        std::fs::create_dir_all("logs")?;
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let path = PathBuf::from(format!("logs/game-{secs}.log"));
        Ok(GameLog { file: Mutex::new(std::fs::File::create(&path)?), path, start: std::time::Instant::now() })
    }

    pub fn line(&self, text: impl AsRef<str>) {
        let line = format!("[{:8.2}s] {}", self.start.elapsed().as_secs_f64(), text.as_ref());
        println!("{line}");
        if let Ok(mut f) = self.file.lock() {
            let _ = writeln!(f, "{line}");
        }
    }
}
