//! UDP packets between the TS plugin and the overlay on the local network.
//!
//! The plugin broadcasts a snapshot to `PORT`; the overlay sends volume commands back to the
//! plugin's `CONTROL_PORT`. Plain UTF-8 lines; the nickname is always the last field, so it may
//! contain spaces:
//! ```text
//! TSLANOVERLAY2
//! me <self clid>
//! ch <our channel id>
//! c <clid> <nickname>                         one per client in our channel
//! t <clid> <0|1 whisper> <volume dB> <nickname>   one per client talking right now
//! vol <clid> <volume dB>                      a command, alone in its packet
//! ```
//! The version in the magic line changes whenever the format does, so a plugin and an overlay
//! of different versions ignore each other instead of misreading.

pub const PORT: u16 = 39391;
pub const CONTROL_PORT: u16 = 39392;
const MAGIC: &str = "TSLANOVERLAY2";

#[derive(Default, Clone, PartialEq, Debug)]
pub struct Snapshot {
    pub self_id: u16,
    pub channel_id: u64,
    /// Everyone in our channel: (clid, nickname). Empty = not connected to the server.
    pub channel: Vec<(u16, String)>,
    /// Who is talking right now: (clid, whisper, our volume for them in dB, nickname).
    pub talking: Vec<(u16, bool, i32, String)>,
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
        let mut s = format!("{MAGIC}\nme {}\nch {}\n", self.self_id, self.channel_id);
        for (id, name) in &self.channel {
            s += &format!("c {id} {}\n", clean(name));
        }
        for (id, whisper, volume, name) in &self.talking {
            s += &format!("t {id} {} {volume} {}\n", *whisper as u8, clean(name));
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
                "me" => snap.self_id = id.parse().ok()?,
                "ch" => snap.channel_id = id.parse().ok()?,
                "c" => snap.channel.push((id.parse().ok()?, rest.to_string())),
                "t" => {
                    let (whisper, rest) = field(rest)?;
                    let (volume, name) = field(rest)?;
                    snap.talking.push((id.parse().ok()?, whisper == "1", volume.parse().ok()?, name.to_string()));
                }
                _ => {}
            }
        }
        Some(snap)
    }
}

/// "Set our volume for this client to `db`".
pub fn encode_volume(clid: u16, db: i32) -> Vec<u8> {
    format!("{MAGIC}\nvol {clid} {db}\n").into_bytes()
}

pub fn decode_volume(buf: &[u8]) -> Option<(u16, i32)> {
    let mut lines = std::str::from_utf8(buf).ok()?.lines();
    if lines.next()? != MAGIC {
        return None;
    }
    let (kind, rest) = field(lines.next()?)?;
    let (id, db) = field(rest)?;
    (kind == "vol").then_some(())?;
    Some((id.parse().ok()?, db.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let snap = Snapshot {
            self_id: 7,
            channel_id: 4_000_000_123,
            channel: vec![(7, "Я сам".into()), (12, "ник с  пробелами".into())],
            talking: vec![(12, true, -6, "ник с  пробелами".into()), (3, false, 0, "Bob".into())],
        };
        assert_eq!(Snapshot::decode(&snap.encode()), Some(snap));
        assert_eq!(Snapshot::decode(&Snapshot::default().encode()), Some(Snapshot::default()));
        assert_eq!(Snapshot::decode(b"garbage"), None);
        assert_eq!(Snapshot::decode(b"TSLANOVERLAY1\nme 1\n"), None);
    }

    #[test]
    fn volume_command() {
        assert_eq!(decode_volume(&encode_volume(12, -15)), Some((12, -15)));
        assert_eq!(decode_volume(&Snapshot::default().encode()), None);
        assert_eq!(decode_volume(b"garbage"), None);
    }
}
