//! TS LAN Overlay: two always-on-top click-through overlays fed over the local network by the
//! `ts3_lan_overlay` TeamSpeak plugin running on another computer.
//!
//! "Кто говорит" lists current speakers, whispers on top and channel voice below; a nick
//! lingers for a while after the person stops. "Список канала" lists everyone in the channel.
//! Settings and log live in %APPDATA%\TS LAN Overlay; the tray icon opens settings or exits.

#![windows_subsystem = "windows"]

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use proto::{CONTROL_PORT, PORT, Snapshot};

mod config;
mod overlay;
mod settings;

pub const NAME: &str = "TS LAN Overlay";
/// No packet for this long = the TS computer is gone; the overlays are emptied.
const SILENCE: Duration = Duration::from_secs(3);
/// Range of the per-client volume the overlay lets you set, dB (the plugin clamps too).
const VOLUME_RANGE: (i32, i32) = (-40, 20);

pub struct Speaker {
    pub name: String,
    pub talking: bool,
    pub whisper: bool,
    /// When they started talking, or when they stopped if `talking` is false.
    pub since: Instant,
    /// The TS computer's local volume adjustment for them, dB.
    pub volume: i32,
}

#[derive(Default)]
pub struct State {
    pub self_id: u16,
    pub channel_id: u64,
    pub speakers: HashMap<u16, Speaker>,
    /// Everyone in the channel: (clid, nickname).
    pub channel: Vec<(u16, String)>,
    /// When someone came into our channel, while they are highlighted.
    pub joined: HashMap<u16, Instant>,
}

pub static STATE: LazyLock<Mutex<State>> = LazyLock::new(Default::default);
/// Where the last snapshot came from; volume commands go back there.
static TS_ADDR: Mutex<Option<IpAddr>> = Mutex::new(None);
pub static DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    let dir = PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join(NAME);
    let _ = std::fs::create_dir_all(&dir);
    dir
});

pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn log(msg: &str) {
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(DIR.join("overlay.log")) {
        let _ = writeln!(f, "{msg}");
    }
}

/// Folds a snapshot from the plugin into `STATE`: people who stopped talking start lingering.
fn apply(snap: &Snapshot) {
    let now = Instant::now();
    let mut s = lock(&STATE);
    if snap.channel.is_empty() {
        s.speakers.clear(); // left the server
    }
    for (id, sp) in s.speakers.iter_mut() {
        if sp.talking && !snap.talking.iter().any(|t| t.0 == *id) {
            sp.talking = false;
            sp.since = now; // starts the linger countdown
        }
    }
    for (id, whisper, volume, name) in &snap.talking {
        match s.speakers.get_mut(id) {
            Some(sp) if sp.talking && sp.whisper == *whisper => (sp.name, sp.volume) = (name.clone(), *volume),
            _ => {
                let speaker = Speaker { name: name.clone(), talking: true, whisper: *whisper, since: now, volume: *volume };
                s.speakers.insert(*id, speaker);
            }
        }
    }
    // Only people arriving in the channel we already sit in; not everyone on connect or when
    // we switch channels ourselves.
    if !s.channel.is_empty() && s.channel_id == snap.channel_id {
        for (id, _) in &snap.channel {
            if !s.channel.iter().any(|c| c.0 == *id) {
                s.joined.insert(*id, now);
            }
        }
    }
    s.joined.retain(|id, _| snap.channel.iter().any(|c| c.0 == *id));
    s.self_id = snap.self_id;
    s.channel_id = snap.channel_id;
    s.channel = snap.channel.clone();
}

/// Asks the TS computer to set its volume for a client; shown here right away.
/// Called from the overlay thread.
pub fn set_volume(clid: u16, db: i32) {
    let db = db.clamp(VOLUME_RANGE.0, VOLUME_RANGE.1);
    let Some(ip) = *lock(&TS_ADDR) else { return };
    let sent = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).and_then(|s| s.send_to(&proto::encode_volume(clid, db), (ip, CONTROL_PORT)));
    if let Err(e) = sent {
        return log(&format!("не удалось отправить громкость на {ip}: {e}"));
    }
    if let Some(sp) = lock(&STATE).speakers.get_mut(&clid) {
        sp.volume = db;
    }
    overlay::refresh();
}

fn run_receiver(sock: UdpSocket) {
    let _ = sock.set_read_timeout(Some(Duration::from_secs(1)));
    let mut buf = vec![0u8; 65536];
    let mut last: Option<Snapshot> = None;
    let mut heard = Instant::now();
    let mut connected = false;
    loop {
        let snap = match sock.recv_from(&mut buf) {
            Ok((n, from)) => match Snapshot::decode(&buf[..n]) {
                Some(snap) => {
                    heard = Instant::now();
                    *lock(&TS_ADDR) = Some(from.ip());
                    // a TS computer on several networks sends from each address; log the first
                    if !connected {
                        log(&format!("получаю данные от {}", from.ip()));
                        connected = true;
                    }
                    snap
                }
                None => continue,
            },
            Err(_) if heard.elapsed() >= SILENCE && last.is_some() => {
                log("нет данных от TeamSpeak, окна очищены");
                connected = false;
                Snapshot::default()
            }
            Err(_) => continue,
        };
        if last.as_ref() != Some(&snap) {
            apply(&snap);
            overlay::refresh();
        }
        // after silence, `last` stays None so the message above is logged once
        last = (heard.elapsed() < SILENCE).then_some(snap);
    }
}

fn main() {
    // also keeps a second copy from starting: only one program can own the port
    let sock = match UdpSocket::bind((Ipv4Addr::UNSPECIFIED, PORT)) {
        Ok(s) => s,
        Err(e) => {
            log(&format!("не удалось открыть порт UDP {PORT}: {e}"));
            overlay::message(&format!(
                "Не удалось открыть порт UDP {PORT}: {e}\n\nВозможно, {NAME} уже запущен (значок в трее)."
            ));
            return;
        }
    };
    log(&format!("запущен, жду данные на порту UDP {PORT}"));
    std::thread::spawn(move || run_receiver(sock));
    overlay::run();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(channel_id: u64, ids: &[u16]) -> Snapshot {
        Snapshot { self_id: 1, channel_id, channel: ids.iter().map(|&i| (i, format!("n{i}"))).collect(), talking: vec![] }
    }

    #[test]
    fn highlights_only_people_joining_our_channel() {
        apply(&snap(5, &[1, 2])); // connected: nobody highlighted
        assert!(lock(&STATE).joined.is_empty());
        apply(&snap(5, &[1, 2, 3])); // 3 came in
        assert_eq!(lock(&STATE).joined.keys().copied().collect::<Vec<_>>(), vec![3]);
        apply(&snap(5, &[1, 2])); // 3 left again
        assert!(lock(&STATE).joined.is_empty());
        apply(&snap(9, &[1, 7, 8])); // we moved to another channel
        assert!(lock(&STATE).joined.is_empty());
    }
}
