//! TS LAN Overlay: two always-on-top click-through overlays fed over the local network by the
//! `ts3_lan_overlay` TeamSpeak plugin running on another computer.
//!
//! "Кто говорит" lists current speakers of every TS tab, whispers on top and channel voice below; a nick
//! lingers for a while after the person stops. "Список канала" lists everyone in the channel.
//! Settings and log live in %APPDATA%\TS LAN Overlay; the tray icon opens settings or exits.

#![windows_subsystem = "windows"]

use std::collections::{BTreeMap, HashMap};
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

/// One connected server tab on the TS computer.
#[derive(Default)]
pub struct Tab {
    /// Server address, for the tab's heading in the overlays.
    pub name: String,
    pub self_id: u16,
    pub channel_id: u64,
    pub speakers: HashMap<u16, Speaker>,
    /// Everyone in our channel: (clid, nickname).
    pub channel: Vec<(u16, String)>,
    /// When someone came into our channel, while they are highlighted.
    pub joined: HashMap<u16, Instant>,
}

#[derive(Default)]
pub struct State {
    /// The tab in front in TS; shown first.
    pub active: u64,
    /// Every connected tab by its id on the TS computer.
    pub tabs: BTreeMap<u64, Tab>,
}

impl State {
    /// Tabs in display order: the active one first, the rest as TS opened them.
    pub fn ordered(&self) -> impl Iterator<Item = (u64, &Tab)> {
        let first = self.tabs.get_key_value(&self.active).map(|(k, t)| (*k, t));
        first.into_iter().chain(self.tabs.iter().filter(|(k, _)| **k != self.active).map(|(k, t)| (*k, t)))
    }
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
    // tabs closed or disconnected on the TS computer go away with their speakers
    s.tabs.retain(|id, _| snap.tabs.iter().any(|t| t.id == *id));
    s.active = snap.tabs.iter().find(|t| t.active).map_or(0, |t| t.id);
    for st in &snap.tabs {
        let tab = s.tabs.entry(st.id).or_default();
        for (id, sp) in tab.speakers.iter_mut() {
            if sp.talking && !st.talking.iter().any(|t| t.0 == *id) {
                sp.talking = false;
                sp.since = now; // starts the linger countdown
            }
        }
        for (id, whisper, volume, name) in &st.talking {
            match tab.speakers.get_mut(id) {
                Some(sp) if sp.talking && sp.whisper == *whisper => (sp.name, sp.volume) = (name.clone(), *volume),
                _ => {
                    let speaker = Speaker { name: name.clone(), talking: true, whisper: *whisper, since: now, volume: *volume };
                    tab.speakers.insert(*id, speaker);
                }
            }
        }
        // Only people arriving in the channel we already sit in; not everyone on connect or when
        // we switch channels ourselves.
        if !tab.channel.is_empty() && tab.channel_id == st.channel_id {
            for (id, _) in &st.channel {
                if !tab.channel.iter().any(|c| c.0 == *id) {
                    tab.joined.insert(*id, now);
                }
            }
        }
        tab.joined.retain(|id, _| st.channel.iter().any(|c| c.0 == *id));
        (tab.name, tab.self_id, tab.channel_id) = (st.name.clone(), st.self_id, st.channel_id);
        tab.channel = st.channel.clone();
    }
}

/// Asks the TS computer to set its volume for a client on one of its tabs; shown here right
/// away. Called from the overlay thread.
pub fn set_volume(sch: u64, clid: u16, db: i32) {
    let db = db.clamp(VOLUME_RANGE.0, VOLUME_RANGE.1);
    let Some(ip) = *lock(&TS_ADDR) else { return };
    let packet = proto::encode_volume(sch, clid, db);
    let sent = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).and_then(|s| s.send_to(&packet, (ip, CONTROL_PORT)));
    if let Err(e) = sent {
        return log(&format!("не удалось отправить громкость на {ip}: {e}"));
    }
    if let Some(sp) = lock(&STATE).tabs.get_mut(&sch).and_then(|t| t.speakers.get_mut(&clid)) {
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

    fn tab(id: u64, channel_id: u64, ids: &[u16]) -> proto::Tab {
        proto::Tab {
            id,
            active: id == 1,
            name: format!("server {id}"),
            self_id: 1,
            channel_id,
            channel: ids.iter().map(|&i| (i, format!("n{i}"))).collect(),
            talking: vec![],
        }
    }

    fn snap(channel_id: u64, ids: &[u16]) -> Snapshot {
        Snapshot { tabs: vec![tab(1, channel_id, ids)] }
    }

    fn joined(tab: u64) -> Vec<u16> {
        lock(&STATE).tabs.get(&tab).map(|t| t.joined.keys().copied().collect()).unwrap_or_default()
    }

    #[test]
    fn highlights_only_people_joining_our_channel() {
        apply(&snap(5, &[1, 2])); // connected: nobody highlighted
        assert!(joined(1).is_empty());
        apply(&snap(5, &[1, 2, 3])); // 3 came in
        assert_eq!(joined(1), vec![3]);
        apply(&snap(5, &[1, 2])); // 3 left again
        assert!(joined(1).is_empty());
        apply(&snap(9, &[1, 7, 8])); // we moved to another channel
        assert!(joined(1).is_empty());

        // a second tab opens: its people are not "joining", the same clid there is someone else
        let mut two = snap(9, &[1, 7, 8]);
        two.tabs.push(tab(2, 4, &[1, 3]));
        apply(&two);
        assert!(joined(2).is_empty());
        two.tabs[1].channel.push((7, "n7".into()));
        two.tabs[1].talking.push((7, true, -3, "n7".into()));
        apply(&two);
        assert_eq!((joined(1), joined(2)), (vec![], vec![7]));
        assert!(lock(&STATE).tabs[&2].speakers[&7].whisper);
        assert!(!lock(&STATE).tabs[&1].speakers.contains_key(&7));

        // the second tab disconnects
        apply(&snap(9, &[1, 7, 8]));
        assert_eq!(lock(&STATE).tabs.keys().copied().collect::<Vec<_>>(), vec![1]);
        assert_eq!(lock(&STATE).active, 1);
    }
}
