//! UDP packets between the TS plugin and the overlay on the local network.
//!
//! The plugin broadcasts a snapshot to `PORT`; the overlay sends volume commands back to the
//! plugin's `CONTROL_PORT`. Plain UTF-8 lines; the nickname or server address is always the last
//! field, so it may contain spaces:
//! ```text
//! TSLANOVERLAY3
//! tab <tab id> <0|1 active> <self clid> <our channel id> <server address>   one per connected tab
//! c <clid> <nickname>                         one per client in our channel on the tab above
//! t <clid> <0|1 whisper> <volume dB> <nickname>   one per client talking right now on it
//! vol <tab id> <clid> <volume dB>             a command, alone in its packet
//! ```
//! The version in the magic line changes whenever the format does, so a plugin and an overlay
//! of different versions ignore each other instead of misreading.

pub const PORT: u16 = 39391;
pub const CONTROL_PORT: u16 = 39392;
const MAGIC: &str = "TSLANOVERLAY3";

/// One connected server tab.
#[derive(Default, Clone, PartialEq, Debug)]
pub struct Tab {
    /// TS's server connection handler id; clids are only unique within a tab.
    pub id: u64,
    /// The tab in front in TS.
    pub active: bool,
    /// Server address as TS shows it on the tab, for the tab's heading.
    pub name: String,
    pub self_id: u16,
    pub channel_id: u64,
    /// Everyone in our channel: (clid, nickname).
    pub channel: Vec<(u16, String)>,
    /// Who is talking right now: (clid, whisper, our volume for them in dB, nickname).
    pub talking: Vec<(u16, bool, i32, String)>,
}

/// Every connected tab. No tabs = TS is not connected to any server.
#[derive(Default, Clone, PartialEq, Debug)]
pub struct Snapshot {
    pub tabs: Vec<Tab>,
}

fn clean(name: &str) -> String {
    name.replace(['\r', '\n'], " ")
}

/// Splits off the first space-separated field.
fn field(s: &str) -> Option<(&str, &str)> {
    Some(s.split_once(' ').unwrap_or((s, "")))
}

impl Snapshot {
    pub fn encode(&self) -> Vec<u8> {
        let mut s = format!("{MAGIC}\n");
        for t in &self.tabs {
            s += &format!("tab {} {} {} {} {}\n", t.id, t.active as u8, t.self_id, t.channel_id, clean(&t.name));
            for (id, name) in &t.channel {
                s += &format!("c {id} {}\n", clean(name));
            }
            for (id, whisper, volume, name) in &t.talking {
                s += &format!("t {id} {} {volume} {}\n", *whisper as u8, clean(name));
            }
        }
        s.into_bytes()
    }

    pub fn decode(buf: &[u8]) -> Option<Snapshot> {
        let mut lines = std::str::from_utf8(buf).ok()?.lines();
        if lines.next()? != MAGIC {
            return None;
        }
        let mut snap = Snapshot::default();
        for line in lines {
            let (kind, rest) = field(line)?;
            let (id, rest) = field(rest)?;
            match kind {
                "tab" => {
                    let (active, rest) = field(rest)?;
                    let (me, rest) = field(rest)?;
                    let (ch, name) = field(rest)?;
                    snap.tabs.push(Tab {
                        id: id.parse().ok()?,
                        active: active == "1",
                        name: name.to_string(),
                        self_id: me.parse().ok()?,
                        channel_id: ch.parse().ok()?,
                        ..Default::default()
                    });
                }
                "c" => snap.tabs.last_mut()?.channel.push((id.parse().ok()?, rest.to_string())),
                "t" => {
                    let (whisper, rest) = field(rest)?;
                    let (volume, name) = field(rest)?;
                    let talking = (id.parse().ok()?, whisper == "1", volume.parse().ok()?, name.to_string());
                    snap.tabs.last_mut()?.talking.push(talking);
                }
                _ => {}
            }
        }
        Some(snap)
    }
}

/// "Set our volume for this client on this tab to `db`".
pub fn encode_volume(tab: u64, clid: u16, db: i32) -> Vec<u8> {
    format!("{MAGIC}\nvol {tab} {clid} {db}\n").into_bytes()
}

pub fn decode_volume(buf: &[u8]) -> Option<(u64, u16, i32)> {
    let mut lines = std::str::from_utf8(buf).ok()?.lines();
    if lines.next()? != MAGIC {
        return None;
    }
    let (kind, rest) = field(lines.next()?)?;
    let (tab, rest) = field(rest)?;
    let (id, db) = field(rest)?;
    (kind == "vol").then_some(())?;
    Some((tab.parse().ok()?, id.parse().ok()?, db.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let snap = Snapshot {
            tabs: vec![
                Tab {
                    id: 1,
                    active: false,
                    name: "Сервер  с пробелами".into(),
                    self_id: 7,
                    channel_id: 4_000_000_123,
                    channel: vec![(7, "Я сам".into()), (12, "ник с  пробелами".into())],
                    talking: vec![(12, true, -6, "ник с  пробелами".into()), (3, false, 0, "Bob".into())],
                },
                Tab { id: 2, active: true, name: "Другой".into(), self_id: 3, channel_id: 9, channel: vec![(3, "Я".into())], talking: vec![] },
            ],
        };
        assert_eq!(Snapshot::decode(&snap.encode()), Some(snap));
        assert_eq!(Snapshot::decode(&Snapshot::default().encode()), Some(Snapshot::default()));
        assert_eq!(Snapshot::decode(b"garbage"), None);
        assert_eq!(Snapshot::decode(b"TSLANOVERLAY2\nme 1\n"), None);
        assert_eq!(Snapshot::decode(b"TSLANOVERLAY3\nc 1 orphan\n"), None, "a client line needs a tab line before it");
    }

    #[test]
    fn volume_command() {
        assert_eq!(decode_volume(&encode_volume(2, 12, -15)), Some((2, 12, -15)));
        assert_eq!(decode_volume(&Snapshot::default().encode()), None);
        assert_eq!(decode_volume(b"garbage"), None);
    }
}
