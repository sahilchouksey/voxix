use crate::settings::{self, AppSettings};
use cpal::traits::{DeviceTrait, HostTrait};
use log::{debug, error, warn};
use rodio::OutputStreamBuilder;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::thread;
use tauri::{AppHandle, Manager};

pub enum SoundType {
    Start,
    Stop,
    Paste,
    Cancel,
}

fn resolve_sound_path(
    app: &AppHandle,
    _settings: &AppSettings,
    sound_type: SoundType,
) -> Option<PathBuf> {
    let sound_file = get_sound_path(_settings, sound_type);

    // Try resource_dir first (works in both dev and production)
    if let Ok(resource_dir) = app.path().resource_dir() {
        let path = resource_dir.join("resources").join(&sound_file);
        if path.exists() {
            return Some(path);
        }
        // Also try without "resources/" prefix (dev mode layout)
        let path2 = resource_dir.join(&sound_file);
        if path2.exists() {
            return Some(path2);
        }
    }

    // Fallback: resolve via BaseDirectory::Resource
    if let Ok(path) = app
        .path()
        .resolve(&sound_file, tauri::path::BaseDirectory::Resource)
    {
        if path.exists() {
            return Some(path);
        }
    }

    // Fallback: try CARGO_MANIFEST_DIR/resources/ (dev mode)
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        let stem = Path::new(&sound_file);
        let fallback = PathBuf::from(manifest).join("resources").join(stem);
        if fallback.exists() {
            debug!("sound file fallback: {:?}", fallback);
            return Some(fallback);
        }
    }

    None
}

fn get_sound_path(_settings: &AppSettings, sound_type: SoundType) -> String {
    match sound_type {
        SoundType::Start => "audio/startRecording.mp3".to_string(),
        SoundType::Stop => "audio/stopRecording.mp3".to_string(),
        SoundType::Paste => "audio/pasteTranscript.mp3".to_string(),
        SoundType::Cancel => "audio/cancel.mp3".to_string(),
    }
}

pub fn play_feedback_sound(app: &AppHandle, sound_type: SoundType) {
    let settings = settings::get_settings(app);
    if !settings.audio_feedback {
        return;
    }
    if let Some(path) = resolve_sound_path(app, &settings, sound_type) {
        play_sound_async(app, path);
    }
}

pub fn play_feedback_sound_blocking(app: &AppHandle, sound_type: SoundType) {
    let settings = settings::get_settings(app);
    if !settings.audio_feedback {
        return;
    }
    if let Some(path) = resolve_sound_path(app, &settings, sound_type) {
        play_sound_blocking(app, &path);
    }
}

pub fn play_test_sound(app: &AppHandle, sound_type: SoundType) {
    let settings = settings::get_settings(app);
    if let Some(path) = resolve_sound_path(app, &settings, sound_type) {
        play_sound_blocking(app, &path);
    }
}

fn play_sound_async(app: &AppHandle, path: PathBuf) {
    let app_handle = app.clone();
    thread::spawn(move || {
        if let Err(e) = play_sound_at_path(&app_handle, path.as_path()) {
            error!("Failed to play sound '{}': {}", path.display(), e);
        }
    });
}

fn play_sound_blocking(app: &AppHandle, path: &Path) {
    if let Err(e) = play_sound_at_path(app, path) {
        error!("Failed to play sound '{}': {}", path.display(), e);
    }
}

fn play_sound_at_path(app: &AppHandle, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let settings = settings::get_settings(app);
    let volume = settings.audio_feedback_volume;
    let selected_device = settings.selected_output_device.clone();
    play_audio_file(path, selected_device, volume)
}

fn play_audio_file(
    path: &std::path::Path,
    selected_device: Option<String>,
    volume: f32,
) -> Result<(), Box<dyn std::error::Error>> {
    let stream_builder = if let Some(device_name) = selected_device {
        if device_name == "Default" {
            debug!("Using default device");
            OutputStreamBuilder::from_default_device()?
        } else {
            let host = crate::audio_toolkit::get_cpal_host();
            let devices = host.output_devices()?;

            let mut found_device = None;
            for device in devices {
                if device.name()? == device_name {
                    found_device = Some(device);
                    break;
                }
            }

            match found_device {
                Some(device) => OutputStreamBuilder::from_device(device)?,
                None => {
                    warn!("Device '{}' not found, using default device", device_name);
                    OutputStreamBuilder::from_default_device()?
                }
            }
        }
    } else {
        debug!("Using default device");
        OutputStreamBuilder::from_default_device()?
    };

    let stream_handle = stream_builder.open_stream()?;
    let mixer = stream_handle.mixer();

    let file = File::open(path)?;
    let buf_reader = BufReader::new(file);

    let sink = rodio::play(mixer, buf_reader)?;
    sink.set_volume(volume);
    sink.sleep_until_end();

    Ok(())
}
