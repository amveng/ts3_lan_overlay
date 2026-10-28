//! The UDP packet the TS plugin broadcasts on the local network and the overlay listens for.
//!
//! Plain UTF-8 lines; the nickname is always the last field, so it may contain spaces:
//! ```text
//! TSLANOVERLAY1
//! me <self clid>
//! c <clid> <nickname>                 one per client in our channel
//! t <clid> <0|1 whisper> <nickname>   one per client talking right now
//! ```

pub const PORT: u16 = 39391;
const MAGIC: &str = "TSLANOVERLAY1";

#[derive(Default, Clone, PartialEq, Debug)]
pub struct Snapshot {
    pub self_id: u16,
    /// Everyone in our channel: (clid, nickname). Empty = not connected to the server.
    pub channel: Vec<(u16, String)>,
    /// Who is talking right now: (clid, whisper, nickname).
    pub talking: Vec<(u16, bool, String)>,
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
        let mut s = format!("{MAGIC}\nme {}\n", self.self_id);
        for (id, name) in &self.channel {
            s += &format!("c {id} {}\n", clean(name));
        }
        for (id, whisper, name) in &self.talking {
            s += &format!("t {id} {} {}\n", *whisper as u8, clean(name));
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
            let id = id.parse().ok()?;
            match kind {
                "me" => snap.self_id = id,
                "c" => snap.channel.push((id, rest.to_string())),
                "t" => {
                    let (whisper, name) = field(rest)?;
                    snap.talking.push((id, whisper == "1", name.to_string()));
                }
                _ => {}
            }
        }
        Some(snap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let snap = Snapshot {
            self_id: 7,
            channel: vec![(7, "Я сам".into()), (12, "ник с  пробелами".into())],
            talking: vec![(12, true, "ник с  пробелами".into()), (3, false, "Bob".into())],
        };
        assert_eq!(Snapshot::decode(&snap.encode()), Some(snap));
        assert_eq!(Snapshot::decode(&Snapshot::default().encode()), Some(Snapshot::default()));
        assert_eq!(Snapshot::decode(b"garbage"), None);
    }
}
