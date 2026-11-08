//! TeamSpeak 3 plugin: broadcasts who is talking and who is in our channel on the local network,
//! for the TS LAN Overlay program on another computer to draw.
//!
//! A snapshot goes out right after every change and once a second anyway, as UDP broadcast on
//! every local IPv4 address. Covers every connected server tab; with none it broadcasts an
//! empty snapshot. The overlay sends volume commands back
//! to `CONTROL_PORT`. Log in `ts3_lan_overlay.log` next to the DLL.

use std::collections::{BTreeMap, HashMap};
use std::ffi::{CStr, c_char, c_int, c_uint, c_void};
use std::fs::OpenOptions;
use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use proto::{CONTROL_PORT, PORT, Snapshot};

const NAME: &str = "ts3_lan_overlay";
const STATUS_TALKING: c_int = 1;
const STATUS_DISCONNECTED: c_int = 0;
const STATUS_CONNECTION_ESTABLISHED: c_int = 4;
const LEAVE_VISIBILITY: c_int = 2;
const CLIENT_UNIQUE_IDENTIFIER: usize = 0;
const CLIENT_NICKNAME: usize = 1;
const CLIENT_VOLUME_MODIFICATOR: usize = 18;
/// Range of the per-client volume the overlay may set, dB.
const VOLUME_RANGE: (i32, i32) = (-40, 20);
const HEARTBEAT: Duration = Duration::from_secs(1);
/// How often the list of local addresses is re-read (network adapters come and go).
const RESCAN: Duration = Duration::from_secs(10);

/// The few TS3Functions we call, by index in `struct TS3Functions` (ts3_functions.h, API 26).
struct Ts3Fns {
    free_memory: unsafe extern "C" fn(*mut c_void) -> c_uint,
    set_client_volume_modifier: unsafe extern "C" fn(u64, u16, f32) -> c_uint,
    get_client_id: unsafe extern "C" fn(u64, *mut u16) -> c_uint,
    get_client_variable_as_int: unsafe extern "C" fn(u64, u16, usize, *mut c_int) -> c_uint,
    get_client_variable_as_string: unsafe extern "C" fn(u64, u16, usize, *mut *mut c_char) -> c_uint,
    get_channel_of_client: unsafe extern "C" fn(u64, u16, *mut u64) -> c_uint,
    get_channel_client_list: unsafe extern "C" fn(u64, u64, *mut *mut u16) -> c_uint,
    get_current_server_connection_handler_id: unsafe extern "C" fn() -> u64,
    get_server_connect_info: unsafe extern "C" fn(u64, *mut c_char, *mut u16, *mut c_char, usize) -> c_uint,
    get_server_connection_handler_list: unsafe extern "C" fn(*mut *mut u64) -> c_uint,
}

static TS3: OnceLock<Ts3Fns> = OnceLock::new();

/// One connected server tab.
#[derive(Default)]
struct Tab {
    /// Server address as TS shows it on the tab, for the tab's heading in the overlays.
    name: String,
    /// The address last logged for this tab.
    host: String,
    self_id: u16,
    channel_id: u64,
    /// Everyone in our channel: (clid, nickname).
    channel: Vec<(u16, String)>,
    /// Talking right now: clid -> (whisper, our volume for them in dB, nickname).
    talking: HashMap<u16, (bool, i32, String)>,
}

#[derive(Default)]
struct State {
    /// The tab in front in TS.
    active: u64,
    /// Every connected tab by server connection handler id.
    tabs: BTreeMap<u64, Tab>,
}

/// Volumes set from the overlay. TS neither remembers setClientVolumeModifier across
/// reconnects nor reflects it in CLIENT_VOLUME_MODIFICATOR, so we keep them ourselves, by
/// unique id, in `ts3_lan_overlay_volumes.txt` next to the DLL.
struct Volumes {
    /// uid -> (our dB, TS's own value when we set it). If TS's value later differs, the
    /// volume was changed in TS's own window and that wins.
    saved: HashMap<String, (i32, i32)>,
    /// Live (tab, clid) we set or re-applied a volume for -> what TS reported right after; a
    /// rejoin gets a new clid.
    echo: HashMap<(u64, u16), i32>,
}

static VOLUMES: LazyLock<Mutex<Volumes>> =
    LazyLock::new(|| Mutex::new(Volumes { saved: load_volumes(), echo: HashMap::new() }));

static STATE: LazyLock<Mutex<State>> = LazyLock::new(Default::default);
static PLUGIN_DIR: LazyLock<PathBuf> = LazyLock::new(plugin_dir);
/// Wakes the sender thread; dropping it stops the thread.
static SENDER: Mutex<Option<(mpsc::Sender<()>, JoinHandle<()>)>> = Mutex::new(None);
static CONTROL: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
static STOP_CONTROL: AtomicBool = AtomicBool::new(false);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn plugin_dir() -> PathBuf {
    use windows_sys::Win32::Foundation::HMODULE;
    use windows_sys::Win32::System::LibraryLoader::{
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
        GetModuleFileNameW, GetModuleHandleExW,
    };
    let mut module: HMODULE = std::ptr::null_mut();
    let mut buf = [0u16; 1024];
    let len = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            plugin_dir as *const () as *const u16,
            &mut module,
        );
        GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32)
    };
    let dll = PathBuf::from(std::ffi::OsString::from_wide(&buf[..len as usize]));
    dll.parent().map(PathBuf::from).unwrap_or_default()
}

fn log(msg: &str) {
    let path = PLUGIN_DIR.join(format!("{NAME}.log"));
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{msg}");
    }
}

// ---- Network ----

/// One broadcast socket per local IPv4 address: Windows sends 255.255.255.255 out of the
/// interface a socket is bound to, so this reaches every network the computer is on.
fn broadcast_sockets() -> Vec<(Ipv4Addr, UdpSocket)> {
    let host = std::env::var("COMPUTERNAME").unwrap_or_default();
    let mut ips: Vec<Ipv4Addr> = (host.as_str(), 0)
        .to_socket_addrs()
        .map(|a| a.filter_map(|a| if let SocketAddr::V4(v) = a { Some(*v.ip()) } else { None }).collect())
        .unwrap_or_default();
    ips.retain(|ip| !ip.is_loopback() && !ip.is_link_local());
    ips.sort();
    ips.dedup();
    if ips.is_empty() {
        ips.push(Ipv4Addr::UNSPECIFIED);
    }
    ips.into_iter()
        .filter_map(|ip| {
            let s = UdpSocket::bind((ip, 0)).ok()?;
            s.set_broadcast(true).ok()?;
            Some((ip, s))
        })
        .collect()
}

fn snapshot() -> Snapshot {
    let s = lock(&STATE);
    let tabs = s
        .tabs
        .iter()
        .map(|(&id, t)| {
            let mut talking: Vec<_> = t.talking.iter().map(|(&id, (w, v, n))| (id, *w, *v, n.clone())).collect();
            talking.sort_by_key(|t| t.0);
            proto::Tab {
                id,
                active: id == s.active,
                name: t.name.clone(),
                self_id: t.self_id,
                channel_id: t.channel_id,
                channel: t.channel.clone(),
                talking,
            }
        })
        .collect();
    Snapshot { tabs }
}

fn run_sender(wake: mpsc::Receiver<()>) {
    let mut socks: Vec<(Ipv4Addr, UdpSocket)> = Vec::new();
    let mut scanned: Option<Instant> = None;
    loop {
        if let Err(RecvTimeoutError::Disconnected) = wake.recv_timeout(HEARTBEAT) {
            return;
        }
        if scanned.is_none_or(|t| t.elapsed() >= RESCAN) {
            let new = broadcast_sockets();
            let ips = |v: &[(Ipv4Addr, UdpSocket)]| v.iter().map(|(ip, _)| *ip).collect::<Vec<_>>();
            if ips(&new) != ips(&socks) {
                log(&format!("рассылаю по сети с адресов {:?}, порт UDP {PORT}", ips(&new)));
            }
            (socks, scanned) = (new, Some(Instant::now()));
        }
        let packet = snapshot().encode();
        for (_, s) in &socks {
            let _ = s.send_to(&packet, (Ipv4Addr::BROADCAST, PORT));
        }
    }
}

/// Receives volume commands from the overlay until the plugin shuts down.
fn run_control() {
    let sock = match UdpSocket::bind((Ipv4Addr::UNSPECIFIED, CONTROL_PORT)) {
        Ok(s) => s,
        Err(e) => return log(&format!("не удалось открыть порт UDP {CONTROL_PORT}, громкость из оверлея не работает: {e}")),
    };
    let _ = sock.set_read_timeout(Some(HEARTBEAT));
    let mut buf = [0u8; 512];
    while !STOP_CONTROL.load(Ordering::Relaxed) {
        if let Ok((n, _)) = sock.recv_from(&mut buf) {
            if let Some((tab, clid, db)) = proto::decode_volume(&buf[..n]) {
                set_volume(tab, clid, db);
            }
        }
    }
}

/// Sends a fresh snapshot now. Safe to call from any thread.
fn changed() {
    if let Some((tx, _)) = lock(&SENDER).as_ref() {
        let _ = tx.send(());
    }
}

// ---- TeamSpeak state ----

fn client_string(sch: u64, clid: u16, flag: usize) -> Option<String> {
    let f = TS3.get()?;
    let mut p: *mut c_char = std::ptr::null_mut();
    if unsafe { (f.get_client_variable_as_string)(sch, clid, flag, &mut p) } != 0 || p.is_null() {
        return None;
    }
    let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
    unsafe { (f.free_memory)(p.cast()) };
    Some(s)
}

fn nickname(sch: u64, clid: u16) -> String {
    client_string(sch, clid, CLIENT_NICKNAME).unwrap_or_else(|| format!("clid {clid}"))
}

fn volumes_path() -> PathBuf {
    PLUGIN_DIR.join(format!("{NAME}_volumes.txt"))
}

/// Lines `uid<TAB>dB<TAB>TS value`; anything else is skipped.
fn load_volumes() -> HashMap<String, (i32, i32)> {
    let text = std::fs::read_to_string(volumes_path()).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let mut p = l.split('\t');
            Some((p.next()?.to_string(), (p.next()?.parse().ok()?, p.next()?.parse().ok()?)))
        })
        .collect()
}

fn save_volumes(saved: &HashMap<String, (i32, i32)>) {
    let mut text = String::from("; Громкость, выставленная из оверлея: уникальный ID, дБ, значение TS на тот момент.\n");
    for (uid, (db, ts)) in saved {
        text.push_str(&format!("{uid}\t{db}\t{ts}\n"));
    }
    if let Err(e) = std::fs::write(volumes_path(), text) {
        log(&format!("не удалось сохранить громкость: {e}"));
    }
}

/// CLIENT_VOLUME_MODIFICATOR: changes only when the volume is set in TS's own UI.
fn ts_volume(sch: u64, clid: u16) -> i32 {
    let Some(f) = TS3.get() else { return 0 };
    let mut v: c_int = 0;
    if unsafe { (f.get_client_variable_as_int)(sch, clid, CLIENT_VOLUME_MODIFICATOR, &mut v) } != 0 {
        return 0;
    }
    v
}

fn set_modifier(sch: u64, clid: u16, db: i32) -> bool {
    let Some(f) = TS3.get() else { return false };
    let err = unsafe { (f.set_client_volume_modifier)(sch, clid, db as f32) };
    if err != 0 {
        log(&format!("не удалось изменить громкость clid {clid}: ошибка {err}"));
    }
    err == 0
}

/// Our volume for a client: the one saved from the overlay, unless it was changed in TS since.
/// It is set in TS again on every look: TS drops a plugin-set volume on a rejoin, and also on
/// its own while the person stays, going back to its remembered value.
fn volume(sch: u64, clid: u16) -> i32 {
    let v = ts_volume(sch, clid);
    let Some(uid) = client_string(sch, clid, CLIENT_UNIQUE_IDENTIFIER) else { return v };
    let mut vols = lock(&VOLUMES);
    let Some(&(ours, base)) = vols.saved.get(&uid) else { return v };
    // TS shows either what it reported right after our own change, or its own remembered value
    // from before it (a new clid, or TS dropped ours). Anything else was set in TS's window.
    let echo = vols.echo.get(&(sch, clid)).copied();
    if v != echo.unwrap_or(base) && v != base {
        log(&format!("громкость {uid} изменена в TeamSpeak: {v} дБ, значение из оверлея {ours} дБ забыто"));
        vols.saved.remove(&uid);
        vols.echo.remove(&(sch, clid));
        save_volumes(&vols.saved);
        return v;
    }
    drop(vols);
    set_modifier(sch, clid, ours);
    let now = ts_volume(sch, clid);
    if echo != Some(v) {
        log(&format!("громкость {uid}: восстановлена {ours} дБ (в TS было {v}, стало {now})"));
    }
    lock(&VOLUMES).echo.insert((sch, clid), now);
    ours
}

/// Sets and saves our local volume for a client on tab `sch`, clamped to `VOLUME_RANGE`.
fn set_volume(sch: u64, clid: u16, db: i32) {
    let db = db.clamp(VOLUME_RANGE.0, VOLUME_RANGE.1);
    let Some(uid) = client_string(sch, clid, CLIENT_UNIQUE_IDENTIFIER) else {
        return log(&format!("не удалось узнать уникальный ID clid {clid}, громкость не изменена"));
    };
    let v = ts_volume(sch, clid);
    // TS's own value from before the overlay touched this person: kept while TS still shows
    // what our previous change left there.
    let base = {
        let vols = lock(&VOLUMES);
        match (vols.saved.get(&uid), vols.echo.get(&(sch, clid))) {
            (Some(&(_, b)), Some(&e)) if v == e => b,
            _ => v,
        }
    };
    if !set_modifier(sch, clid, db) {
        return;
    }
    let echo = ts_volume(sch, clid);
    {
        let mut vols = lock(&VOLUMES);
        vols.saved.insert(uid, (db, base));
        vols.echo.insert((sch, clid), echo);
        save_volumes(&vols.saved);
    }
    if let Some(t) = lock(&STATE).tabs.get_mut(&sch).and_then(|t| t.talking.get_mut(&clid)) {
        t.1 = db;
    }
    changed();
}

/// Our own clid, our channel and everyone in it, or None when not connected.
fn read_channel(sch: u64) -> Option<(u16, u64, Vec<(u16, String)>)> {
    let f = TS3.get()?;
    let (mut me, mut cid, mut list) = (0u16, 0u64, std::ptr::null_mut::<u16>());
    unsafe {
        if (f.get_client_id)(sch, &mut me) != 0
            || (f.get_channel_of_client)(sch, me, &mut cid) != 0
            || (f.get_channel_client_list)(sch, cid, &mut list) != 0
            || list.is_null()
        {
            return None;
        }
        let mut ids = Vec::new();
        let mut p = list;
        while *p != 0 {
            ids.push(*p);
            p = p.add(1);
        }
        (f.free_memory)(list.cast());
        Some((me, cid, ids.into_iter().map(|id| (id, nickname(sch, id))).collect()))
    }
}

/// Host the tab is connected to, or None when it is not connected.
fn server_host(sch: u64) -> Option<String> {
    let f = TS3.get()?;
    let (mut host, mut password, mut port) = ([0 as c_char; 256], [0 as c_char; 256], 0u16);
    let err = unsafe { (f.get_server_connect_info)(sch, host.as_mut_ptr(), &mut port, password.as_mut_ptr(), host.len()) };
    (err == 0).then(|| unsafe { CStr::from_ptr(host.as_ptr()) }.to_string_lossy().trim().to_string())
}

/// All server tabs TS has open, connected or not.
fn tab_ids() -> Vec<u64> {
    let Some(f) = TS3.get() else { return Vec::new() };
    let mut list = std::ptr::null_mut::<u64>();
    if unsafe { (f.get_server_connection_handler_list)(&mut list) } != 0 || list.is_null() {
        return Vec::new();
    }
    let mut ids = Vec::new();
    unsafe {
        let mut p = list;
        while *p != 0 {
            ids.push(*p);
            p = p.add(1);
        }
        (f.free_memory)(list.cast());
    }
    ids
}

/// Re-reads every tab: which are connected, and each one's channel.
fn refresh_all() {
    let Some(f) = TS3.get() else { return };
    let ids = tab_ids();
    let active = unsafe { (f.get_current_server_connection_handler_id)() };
    {
        let mut s = lock(&STATE);
        s.active = active;
        s.tabs.retain(|sch, _| ids.contains(sch));
    }
    lock(&VOLUMES).echo.retain(|(sch, _), _| ids.contains(sch));
    for sch in ids {
        refresh_tab(sch);
    }
    changed();
}

/// Re-reads one tab's channel membership; a tab that is not connected is dropped. TS calls are
/// made before taking the lock.
fn refresh_tab(sch: u64) {
    let Some(host) = server_host(sch) else {
        if lock(&STATE).tabs.remove(&sch).is_some() {
            lock(&VOLUMES).echo.retain(|(s, _), _| *s != sch);
            changed();
        }
        return;
    };
    let Some((me, cid, channel)) = read_channel(sch) else { return };
    // apply saved volumes as people show up in our channel, before they start talking
    for (id, _) in &channel {
        volume(sch, *id);
    }
    {
        let mut s = lock(&STATE);
        let t = s.tabs.entry(sch).or_default();
        if t.host != host {
            log(&format!("вкладка {sch}: сервер {host}"));
            t.host = host.clone();
        }
        (t.name, t.self_id, t.channel_id, t.channel) = (host, me, cid, channel);
    }
    changed();
}

// ---- Required plugin exports ----

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_name() -> *const c_char {
    c"TS LAN Overlay".as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_version() -> *const c_char {
    c"0.3.2".as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_apiVersion() -> c_int {
    26
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_author() -> *const c_char {
    c"TakeChance".as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_description() -> *const c_char {
    c"Sends who is talking and who is in the channel to TS LAN Overlay on another computer in the local network".as_ptr()
}

/// TS passes `struct TS3Functions` by value; on x64 that arrives as a pointer to a temporary
/// copy, so the pointers we need are copied out here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ts3plugin_setFunctionPointers(funcs: *const *const c_void) {
    unsafe {
        let _ = TS3.set(Ts3Fns {
            free_memory: std::mem::transmute(*funcs.add(5)),
            set_client_volume_modifier: std::mem::transmute(*funcs.add(39)),
            get_client_id: std::mem::transmute(*funcs.add(73)),
            get_client_variable_as_int: std::mem::transmute(*funcs.add(79)),
            get_client_variable_as_string: std::mem::transmute(*funcs.add(81)),
            get_channel_of_client: std::mem::transmute(*funcs.add(83)),
            get_channel_client_list: std::mem::transmute(*funcs.add(94)),
            get_current_server_connection_handler_id: std::mem::transmute(*funcs.add(183)),
            get_server_connect_info: std::mem::transmute(*funcs.add(189)),
            get_server_connection_handler_list: std::mem::transmute(*funcs.add(96)),
        });
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_init() -> c_int {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || run_sender(rx));
    *lock(&SENDER) = Some((tx, handle));
    STOP_CONTROL.store(false, Ordering::Relaxed);
    *lock(&CONTROL) = Some(std::thread::spawn(run_control));
    refresh_all();
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_shutdown() {
    // tell the overlay right away instead of letting it time out
    *lock(&STATE) = State::default();
    changed();
    if let Some((tx, handle)) = lock(&SENDER).take() {
        drop(tx);
        let _ = handle.join();
    }
    STOP_CONTROL.store(true, Ordering::Relaxed);
    if let Some(handle) = lock(&CONTROL).take() {
        let _ = handle.join();
    }
}

// ---- Events ----

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_currentServerConnectionChanged(_sch: u64) {
    refresh_all();
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_onConnectStatusChangeEvent(_sch: u64, new_status: c_int, _error: c_uint) {
    if new_status == STATUS_CONNECTION_ESTABLISHED || new_status == STATUS_DISCONNECTED {
        refresh_all();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_onTalkStatusChangeEvent(sch: u64, status: c_int, is_received_whisper: c_int, client_id: u16) {
    let talking = status == STATUS_TALKING;
    let (name, vol) = if talking { (nickname(sch, client_id), volume(sch, client_id)) } else { (String::new(), 0) };
    {
        let mut s = lock(&STATE);
        let Some(t) = s.tabs.get_mut(&sch) else { return };
        if talking {
            t.talking.insert(client_id, (is_received_whisper != 0, vol, name));
        } else {
            t.talking.remove(&client_id);
        }
    }
    changed();
}

fn on_move(sch: u64, client_id: u16, visibility: c_int) {
    if visibility == LEAVE_VISIBILITY {
        if let Some(t) = lock(&STATE).tabs.get_mut(&sch) {
            t.talking.remove(&client_id); // clids get recycled
        }
        lock(&VOLUMES).echo.remove(&(sch, client_id));
    }
    refresh_tab(sch);
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_onClientMoveEvent(sch: u64, client_id: u16, _old: u64, _new: u64, visibility: c_int, _msg: *const c_char) {
    on_move(sch, client_id, visibility);
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_onClientMoveTimeoutEvent(sch: u64, client_id: u16, _old: u64, _new: u64, visibility: c_int, _msg: *const c_char) {
    on_move(sch, client_id, visibility);
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_onClientMoveMovedEvent(
    sch: u64, client_id: u16, _old: u64, _new: u64, visibility: c_int,
    _mover_id: u16, _mover_name: *const c_char, _mover_uid: *const c_char, _msg: *const c_char,
) {
    on_move(sch, client_id, visibility);
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_onClientKickFromChannelEvent(
    sch: u64, client_id: u16, _old: u64, _new: u64, visibility: c_int,
    _kicker_id: u16, _kicker_name: *const c_char, _kicker_uid: *const c_char, _msg: *const c_char,
) {
    on_move(sch, client_id, visibility);
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_onClientKickFromServerEvent(
    sch: u64, client_id: u16, _old: u64, _new: u64, visibility: c_int,
    _kicker_id: u16, _kicker_name: *const c_char, _kicker_uid: *const c_char, _msg: *const c_char,
) {
    on_move(sch, client_id, visibility);
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_onClientDisplayNameChanged(sch: u64, client_id: u16, display_name: *const c_char, _uid: *const c_char) {
    if display_name.is_null() {
        return;
    }
    let name = unsafe { CStr::from_ptr(display_name) }.to_string_lossy().into_owned();
    {
        let mut s = lock(&STATE);
        let Some(tab) = s.tabs.get_mut(&sch) else { return };
        if let Some(t) = tab.talking.get_mut(&client_id) {
            t.2 = name.clone();
        }
        if let Some(c) = tab.channel.iter_mut().find(|(id, _)| *id == client_id) {
            c.1 = name;
        }
    }
    changed();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

    /// Fake TS. CLIENT_VOLUME_MODIFICATOR per live clid; a clid it has no entry for shows
    /// `REMEMBERED`, TS's own stored volume for the person (what a rejoin starts with).
    static VARS: LazyLock<Mutex<HashMap<u16, i32>>> = LazyLock::new(Default::default);
    static REMEMBERED: AtomicI32 = AtomicI32::new(0);
    /// Whether setClientVolumeModifier also updates CLIENT_VOLUME_MODIFICATOR.
    static SET_UPDATES_VAR: AtomicBool = AtomicBool::new(false);
    /// Last value passed to setClientVolumeModifier, i32::MIN = not called.
    static LAST_SET: AtomicI32 = AtomicI32::new(i32::MIN);

    unsafe extern "C" fn get_int(_: u64, clid: u16, _: usize, v: *mut c_int) -> c_uint {
        let val = lock(&VARS).get(&clid).copied().unwrap_or(REMEMBERED.load(Ordering::Relaxed));
        unsafe { *v = val };
        0
    }
    unsafe extern "C" fn set_mod(_: u64, clid: u16, db: f32) -> c_uint {
        LAST_SET.store(db as i32, Ordering::Relaxed);
        if SET_UPDATES_VAR.load(Ordering::Relaxed) {
            lock(&VARS).insert(clid, db as i32);
        }
        0
    }
    /// clid 6 is someone else; every other clid is the same person (rejoins get new clids).
    unsafe extern "C" fn get_str(_: u64, clid: u16, _: usize, out: *mut *mut c_char) -> c_uint {
        let uid = if clid == 6 { "other=" } else { "same+person/uid=" };
        unsafe { *out = CString::new(uid).unwrap().into_raw() };
        0
    }
    unsafe extern "C" fn free(p: *mut c_void) -> c_uint {
        drop(unsafe { CString::from_raw(p.cast()) });
        0
    }
    unsafe extern "C" fn fail_id(_: u64, _: *mut u16) -> c_uint {
        1
    }
    unsafe extern "C" fn fail_chan(_: u64, _: u16, _: *mut u64) -> c_uint {
        1
    }
    unsafe extern "C" fn fail_list(_: u64, _: u64, _: *mut *mut u16) -> c_uint {
        1
    }
    unsafe extern "C" fn sch() -> u64 {
        1
    }
    unsafe extern "C" fn fail_info(_: u64, _: *mut c_char, _: *mut u16, _: *mut c_char, _: usize) -> c_uint {
        1
    }
    unsafe extern "C" fn fail_tabs(_: *mut *mut u64) -> c_uint {
        1
    }

    fn scenario(set_updates_var: bool) {
        let mode = if set_updates_var { "TS updates its variable" } else { "TS keeps its variable" };
        let _ = std::fs::remove_file(volumes_path());
        {
            let mut vols = lock(&VOLUMES);
            vols.saved.clear();
            vols.echo.clear();
        }
        lock(&VARS).clear();
        REMEMBERED.store(0, Ordering::Relaxed);
        SET_UPDATES_VAR.store(set_updates_var, Ordering::Relaxed);

        set_volume(1, 5, -7);
        set_volume(1, 5, -8); // second wheel notch
        assert_eq!(volume(1, 5), -8, "{mode}: talks again");
        assert_eq!(volume(1, 6), 0, "{mode}: other people keep TS's value");

        // rejoin: new clid, TS shows its own remembered value -> ours is applied again
        lock(&VARS).remove(&5);
        LAST_SET.store(i32::MIN, Ordering::Relaxed);
        assert_eq!(volume(1, 15), -8, "{mode}: after rejoin");
        assert_eq!(LAST_SET.load(Ordering::Relaxed), -8, "{mode}: re-applied to TS");

        // TS drops ours on its own while they stay: back to its remembered value
        lock(&VARS).remove(&15);
        LAST_SET.store(i32::MIN, Ordering::Relaxed);
        assert_eq!(volume(1, 15), -8, "{mode}: after TS dropped it");
        assert_eq!(LAST_SET.load(Ordering::Relaxed), -8, "{mode}: set again");
        assert_eq!(load_volumes().get("same+person/uid=").map(|v| v.0), Some(-8), "{mode}: still saved");

        // TS restart: only the file is left
        lock(&VARS).clear();
        {
            let mut vols = lock(&VOLUMES);
            vols.saved = load_volumes();
            vols.echo.clear();
        }
        assert_eq!(volume(1, 25), -8, "{mode}: after TS restart");

        // changed in TS's own volume window while they are here
        lock(&VARS).insert(25, 3);
        REMEMBERED.store(3, Ordering::Relaxed);
        assert_eq!(volume(1, 25), 3, "{mode}: TS window wins");
        assert!(load_volumes().is_empty(), "{mode}: overlay value forgotten");
        lock(&VARS).remove(&25);
        assert_eq!(volume(1, 35), 3, "{mode}: and stays forgotten after rejoin");
    }

    #[test]
    fn volume_from_overlay_is_kept_across_talks_rejoins_and_restarts_until_changed_in_ts() {
        let _ = TS3.set(Ts3Fns {
            free_memory: free,
            set_client_volume_modifier: set_mod,
            get_client_id: fail_id,
            get_client_variable_as_int: get_int,
            get_client_variable_as_string: get_str,
            get_channel_of_client: fail_chan,
            get_channel_client_list: fail_list,
            get_current_server_connection_handler_id: sch,
            get_server_connect_info: fail_info,
            get_server_connection_handler_list: fail_tabs,
        });
        // one test, both modes in turn: they share the plugin's statics
        scenario(true);
        scenario(false);
    }
}
