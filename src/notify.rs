use crate::Config;
use reqwest::blocking::Client;
use serde_json::json;
use std::{
    io::Cursor,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

#[derive(Clone, Copy)]
pub enum Kind {
    Down,
    Up,
    Warn,
    Info,
}

impl Kind {
    fn color(self) -> u32 {
        match self {
            Self::Down => 0xFF6B6B,
            Self::Up => 0x3FD68C,
            Self::Warn => 0xFFC65C,
            Self::Info => 0x7AA2F7,
        }
    }
    fn icon(self) -> &'static str {
        match self {
            Self::Down => "🔴",
            Self::Up => "🟢",
            Self::Warn => "🟡",
            Self::Info => "🔵",
        }
    }
}

#[derive(Clone)]
pub struct Alert {
    pub title: String,
    pub message: String,
    pub kind: Kind,
    pub urgent: bool,
    pub fields: Vec<(String, String)>,
    pub desktop_bmp: Option<Vec<u8>>,
}

pub fn channels(config: &Config) -> Vec<&'static str> {
    let mut result = Vec::new();
    if !config.ntfy_topic.trim().is_empty() {
        result.push("ntfy");
    }
    if !config.webhook_url.trim().is_empty() {
        result.push("discord");
    }
    if !config.telegram_token.trim().is_empty() && !config.telegram_chat_id.trim().is_empty() {
        result.push("telegram");
    }
    if config.sound {
        result.push("sound");
    }
    result
}

fn outcome(name: &str, response: reqwest::Result<reqwest::blocking::Response>) -> String {
    match response {
        Ok(response) if response.status().is_success() => format!("{name}: delivered"),
        Ok(response) => format!("{name}: HTTP {}", response.status().as_u16()),
        Err(error) if error.is_timeout() => format!("{name}: timeout (delivery uncertain)"),
        Err(error) => format!("{name}: failed: {error}"),
    }
}

fn send_once(client: &Client, config: &Config, alert: &Alert, footer: &str) -> Vec<String> {
    let mut results = Vec::new();
    if !config.ntfy_topic.trim().is_empty() {
        let url = format!(
            "{}/{}",
            config.ntfy_server.trim_end_matches('/'),
            config.ntfy_topic.trim()
        );
        let title: String = alert
            .title
            .chars()
            .filter(|c| c.is_ascii_graphic() || *c == ' ')
            .collect();
        let response = client
            .post(url)
            .header(
                "Title",
                if title.trim().is_empty() {
                    "Gamepulse"
                } else {
                    title.trim()
                },
            )
            .header("Priority", if alert.urgent { "urgent" } else { "default" })
            .header(
                "Tags",
                if alert.urgent {
                    "rotating_light"
                } else {
                    "white_check_mark"
                },
            )
            .body(alert.message.clone())
            .send();
        results.push(outcome("ntfy", response));
    }
    if !config.webhook_url.trim().is_empty() {
        results.extend(send_discord(client, config, alert, footer));
    }
    if !config.telegram_token.trim().is_empty() && !config.telegram_chat_id.trim().is_empty() {
        let url = format!(
            "https://api.telegram.org/bot{}/sendMessage",
            config.telegram_token.trim()
        );
        let payload = json!({"chat_id":config.telegram_chat_id,"text":format!("{}\n{}", alert.title, alert.message),"disable_notification":!alert.urgent});
        results.push(outcome("telegram", client.post(url).json(&payload).send()));
    }
    results
}

/// Discord embed (with the heartbeat screenshot when there is one); "@here" only for urgent alerts
fn send_discord(client: &Client, config: &Config, alert: &Alert, footer: &str) -> Vec<String> {
    let mut results = Vec::new();
    {
        let fields: Vec<_> = alert
            .fields
            .iter()
            .map(|(name, value)| json!({"name":name,"value":format!("`{value}`"),"inline":true}))
            .collect();
        let mut payload = json!({"embeds":[{"title":format!("{} {}", alert.kind.icon(), alert.title),"description":alert.message,"color":alert.kind.color(),"fields":fields,"footer":{"text":footer}}]});
        if alert.urgent && config.discord_here {
            payload["content"] = json!("@here");
            payload["allowed_mentions"] = json!({"parse":["everyone"]});
        }
        let separator = if config.webhook_url.contains('?') {
            '&'
        } else {
            '?'
        };
        let url = format!("{}{separator}wait=true", config.webhook_url.trim());
        let attachment = alert.desktop_bmp.as_deref().and_then(|bmp| {
            prepare_attachment(bmp)
                .inspect_err(|error| results.push(format!("screenshot not attached: {error}")))
                .ok()
        });
        let response = if let Some((bytes, filename, mime)) = attachment {
            let form = reqwest::blocking::multipart::Form::new()
                .text("payload_json", payload.to_string())
                .part(
                    "files[0]",
                    reqwest::blocking::multipart::Part::bytes(bytes)
                        .file_name(filename)
                        .mime_str(mime)
                        .expect("static MIME type"),
                );
            client.post(url).multipart(form).send()
        } else {
            client.post(url).json(&payload).send()
        };
        results.push(outcome("discord", response));
    }
    results
}

/// Where MVP alerts go: the Discord webhook and/or a Windows notification (not ntfy/Telegram,
/// which are for waking you up when the game drops)
pub fn boss_channels(config: &Config) -> Vec<&'static str> {
    let mut result = Vec::new();
    if config.boss_discord && !config.webhook_url.trim().is_empty() {
        result.push("discord");
    }
    if config.boss_toast {
        result.push("windows");
    }
    result
}

/// One message per channel: no burst, no repeat, no "@here"
pub fn send_boss(config: Config, alert: Alert) -> Vec<String> {
    let mut results = Vec::new();
    if config.boss_toast {
        results.push(
            match crate::windows::toast::show(&alert.title, &alert.message) {
                Ok(()) => "windows: delivered".into(),
                Err(error) => format!("windows: {error}"),
            },
        );
    }
    if config.boss_discord && !config.webhook_url.trim().is_empty() {
        match Client::builder().timeout(Duration::from_secs(20)).build() {
            Ok(client) => results.extend(send_discord(
                &client,
                &config,
                &alert,
                "Gamepulse · MVP tracker",
            )),
            Err(error) => results.push(format!("HTTP client failed: {error}")),
        }
    }
    results
}

/// Below Discord's attachment limit (10 MiB), leaving room for payload_json
const MAX_ATTACHMENT_BYTES: usize = 8 * 1024 * 1024;

/// Don't send raw BMP: convert to lossless WebP (pure Rust, fast, crisp text),
/// and only fall back to JPEG if it is still too large
fn prepare_attachment(bmp: &[u8]) -> Result<(Vec<u8>, &'static str, &'static str), String> {
    let image = image::load_from_memory_with_format(bmp, image::ImageFormat::Bmp)
        .map_err(|e| format!("cannot decode capture: {e}"))?;
    // GDI alpha is unreliable; drop it before encoding (also smaller files)
    let rgb = image::DynamicImage::ImageRgb8(image.to_rgb8());
    let mut webp = Cursor::new(Vec::new());
    if rgb.write_to(&mut webp, image::ImageFormat::WebP).is_ok()
        && webp.get_ref().len() <= MAX_ATTACHMENT_BYTES
    {
        return Ok((webp.into_inner(), "gamepulse-desktop.webp", "image/webp"));
    }
    let small = rgb.thumbnail(1920, 1080).to_rgb8();
    let mut jpeg = Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 80)
        .encode_image(&small)
        .map_err(|e| format!("cannot encode JPEG: {e}"))?;
    if jpeg.get_ref().len() > MAX_ATTACHMENT_BYTES {
        return Err("screenshot too large even after resizing".into());
    }
    Ok((jpeg.into_inner(), "gamepulse-desktop.jpg", "image/jpeg"))
}

/// Same alarm sound as the old PowerShell version; falls back to the system sound if the file is missing
fn play_alarm() -> &'static str {
    use windows::core::HSTRING;
    use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME, SND_NODEFAULT};
    let media = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into()) + r"\Media\";
    for name in ["Alarm01.wav", "Ring01.wav"] {
        let path = media.clone() + name;
        if std::path::Path::new(&path).exists()
            && unsafe {
                PlaySoundW(
                    &HSTRING::from(path),
                    None,
                    SND_FILENAME | SND_ASYNC | SND_NODEFAULT,
                )
            }
            .as_bool()
        {
            return "sound: played";
        }
    }
    unsafe {
        use windows::Win32::{
            System::Diagnostics::Debug::MessageBeep, UI::WindowsAndMessaging::MB_ICONEXCLAMATION,
        };
        let _ = MessageBeep(MB_ICONEXCLAMATION);
    }
    "sound: system beep"
}

pub fn send(config: Config, alert: Alert) -> Vec<String> {
    send_cancellable(config, alert, None)
}

/// `cancel` is set when the user stops the drill: the rest of the burst must not keep firing
pub fn send_cancellable(
    config: Config,
    mut alert: Alert,
    cancel: Option<&AtomicBool>,
) -> Vec<String> {
    let mut results = Vec::new();
    if alert.title == "Still watching"
        && config.attach_desktop_screenshot
        && !config.webhook_url.trim().is_empty()
    {
        match crate::windows::capture::desktop_bmp() {
            Ok(bytes) => {
                results.push(format!(
                    "heartbeat screenshot captured ({} KiB before encoding)",
                    bytes.len() / 1024
                ));
                alert.desktop_bmp = Some(bytes);
            }
            Err(error) => results.push(format!("heartbeat screenshot unavailable: {error}")),
        }
    }
    if config.sound && alert.urgent {
        results.push(play_alarm().into());
    }
    let client = match Client::builder().timeout(Duration::from_secs(20)).build() {
        Ok(client) => client,
        Err(error) => {
            results.push(format!("HTTP client failed: {error}"));
            return results;
        }
    };
    let count = if alert.urgent {
        config.burst_count.max(1)
    } else {
        1
    };
    for index in 1..=count {
        if index > 1 {
            thread::sleep(Duration::from_secs(config.burst_gap_sec.max(1)));
            if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                results.push(format!(
                    "'{}' stopped after {} of {count}",
                    alert.title,
                    index - 1
                ));
                break;
            }
        }
        let footer = if count > 1 {
            format!("Gamepulse · alert {index} of {count}")
        } else {
            "Gamepulse".into()
        };
        results.extend(send_once(&client, &config, &alert, &footer));
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };

    #[test]
    fn reports_configured_channels_without_exposing_values() {
        let mut config = Config::default();
        assert!(channels(&config).is_empty());
        config.webhook_url = "secret".into();
        config.ntfy_topic = "topic".into();
        assert_eq!(channels(&config), ["ntfy", "discord"]);
        assert_eq!(boss_channels(&config), ["discord", "windows"]);
        config.boss_discord = false;
        config.webhook_url.clear();
        config.boss_toast = false;
        assert!(boss_channels(&config).is_empty());
    }

    #[test]
    fn capture_is_sent_as_webp_not_bmp() {
        let pixels: Vec<u8> = (0..64 * 32).flat_map(|i| [i as u8, 40, 200, 0]).collect();
        let bmp = crate::preview::bmp(64, 32, &pixels).unwrap();
        let (bytes, filename, mime) = prepare_attachment(&bmp).unwrap();
        assert_eq!((filename, mime), ("gamepulse-desktop.webp", "image/webp"));
        assert_eq!((&bytes[..4], &bytes[8..12]), (&b"RIFF"[..], &b"WEBP"[..]));
        let decoded = image::load_from_memory(&bytes).unwrap().to_rgb8();
        assert_eq!(decoded.dimensions(), (64, 32));
        assert_eq!(decoded.get_pixel(0, 0).0, [200, 40, 0]);
        assert!(prepare_attachment(b"not a bmp").is_err());
    }

    #[test]
    fn discord_wait_payload_is_sent_to_mock_server() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = vec![0; 8192];
            let read = stream.read(&mut request).unwrap();
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Type: application/json\r\n\r\n{}").unwrap();
            String::from_utf8_lossy(&request[..read]).into_owned()
        });
        let config = Config {
            webhook_url: format!("http://{address}/webhook"),
            ..Default::default()
        };
        let results = send(
            config,
            Alert {
                title: "Test".into(),
                message: "Delivery".into(),
                kind: Kind::Info,
                urgent: false,
                fields: vec![],
                desktop_bmp: None,
            },
        );
        let request = server.join().unwrap();
        assert!(request.starts_with("POST /webhook?wait=true HTTP/1.1"));
        assert!(request.contains("Delivery"));
        assert_eq!(results, ["discord: delivered"]);
    }
}
