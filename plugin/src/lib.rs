//! TeamSpeak 3 plugin: broadcasts who is talking and who is in our channel on the local network,
//! for the TS LAN Overlay program on another computer to draw.
//!
//! A snapshot goes out right after every change and once a second anyway, as UDP broadcast on
//! every local IPv4 address. Works on the active server tab, and only when it is connected to
//! `SERVER`; otherwise it broadcasts an empty snapshot. Log in `ts3_lan_overlay.log` next to the DLL.

use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_int, c_uint, c_void};
use std::fs::OpenOptions;
use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{LazyLock, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use proto::{PORT, Snapshot};

const NAME: &str = "ts3_lan_overlay";
const STATUS_TALKING: c_int = 1;
const STATUS_DISCONNECTED: c_int = 0;
const STATUS_CONNECTION_ESTABLISHED: c_int = 4;
const LEAVE_VISIBILITY: c_int = 2;
const CLIENT_NICKNAME: usize = 1;
/// The plugin only works on this server; other tabs are ignored.
const SERVER: &str = "irreale.cleanvoice.ru";
const HEARTBEAT: Duration = Duration::from_secs(1);
/// How often the list of local addresses is re-read (network adapters come and go).
const RESCAN: Duration = Duration::from_secs(10);

/// The few TS3Functions we call, by index in `struct TS3Functions` (ts3_functions.h, API 26).
struct Ts3Fns {
    free_memory: unsafe extern "C" fn(*mut c_void) -> c_uint,
    get_client_id: unsafe extern "C" fn(u64, *mut u16) -> c_uint,
    get_client_variable_as_string: unsafe extern "C" fn(u64, u16, usize, *mut *mut c_char) -> c_uint,
    get_channel_of_client: unsafe extern "C" fn(u64, u16, *mut u64) -> c_uint,
    get_channel_client_list: unsafe extern "C" fn(u64, u64, *mut *mut u16) -> c_uint,
    get_current_server_connection_handler_id: unsafe extern "C" fn() -> u64,
    get_server_connect_info: unsafe extern "C" fn(u64, *mut c_char, *mut u16, *mut c_char, usize) -> c_uint,
}

static TS3: OnceLock<Ts3Fns> = OnceLock::new();

#[derive(Default)]
struct State {
    /// Active server tab; events from other tabs are ignored.
    sch: u64,
    /// Host of the active tab, only to log server switches once.
    host: Option<String>,
    self_id: u16,
    channel_id: u64,
    /// Everyone in our channel: (clid, nickname).
    channel: Vec<(u16, String)>,
    /// Talking right now: clid -> (whisper, nickname).
    talking: HashMap<u16, (bool, String)>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(Default::default);
static PLUGIN_DIR: LazyLock<PathBuf> = LazyLock::new(plugin_dir);
/// Wakes the sender thread; dropping it stops the thread.
static SENDER: Mutex<Option<(mpsc::Sender<()>, JoinHandle<()>)>> = Mutex::new(None);

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
    let mut talking: Vec<_> = s.talking.iter().map(|(&id, (w, n))| (id, *w, n.clone())).collect();
    talking.sort_by_key(|t| t.0);
    Snapshot { self_id: s.self_id, channel_id: s.channel_id, channel: s.channel.clone(), talking }
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

/// Sends a fresh snapshot now. Safe to call from any thread.
fn changed() {
    if let Some((tx, _)) = lock(&SENDER).as_ref() {
        let _ = tx.send(());
    }
}

// ---- TeamSpeak state ----

fn nickname(sch: u64, clid: u16) -> String {
    let Some(f) = TS3.get() else { return String::new() };
    let mut p: *mut c_char = std::ptr::null_mut();
    if unsafe { (f.get_client_variable_as_string)(sch, clid, CLIENT_NICKNAME, &mut p) } != 0 || p.is_null() {
        return format!("clid {clid}");
    }
    let name = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
    unsafe { (f.free_memory)(p.cast()) };
    name
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

/// Re-reads the channel membership of the active tab. TS calls are made before taking the lock.
/// A tab on any server other than `SERVER` counts as no tab (sch 0), so nothing is shown.
fn refresh_channel() {
    let Some(f) = TS3.get() else { return };
    let current = unsafe { (f.get_current_server_connection_handler_id)() };
    let host = server_host(current);
    let sch = if host.as_deref().is_some_and(|h| h.eq_ignore_ascii_case(SERVER)) { current } else { 0 };
    let (me, cid, channel) = if sch != 0 { read_channel(sch).unwrap_or_default() } else { Default::default() };
    {
        let mut s = lock(&STATE);
        if s.sch != sch {
            s.talking.clear();
        }
        if s.host != host {
            if let Some(h) = &host {
                log(&format!("вкладка {current}: сервер {h}, {}", if sch != 0 { "работаю" } else { "пропускаю" }));
            }
            s.host = host;
        }
        (s.sch, s.self_id, s.channel_id, s.channel) = (sch, me, cid, channel);
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
    c"0.1.0".as_ptr()
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
            get_client_id: std::mem::transmute(*funcs.add(73)),
            get_client_variable_as_string: std::mem::transmute(*funcs.add(81)),
            get_channel_of_client: std::mem::transmute(*funcs.add(83)),
            get_channel_client_list: std::mem::transmute(*funcs.add(94)),
            get_current_server_connection_handler_id: std::mem::transmute(*funcs.add(183)),
            get_server_connect_info: std::mem::transmute(*funcs.add(189)),
        });
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_init() -> c_int {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || run_sender(rx));
    *lock(&SENDER) = Some((tx, handle));
    refresh_channel();
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
}

// ---- Events ----

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_currentServerConnectionChanged(_sch: u64) {
    refresh_channel();
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_onConnectStatusChangeEvent(_sch: u64, new_status: c_int, _error: c_uint) {
    if new_status == STATUS_CONNECTION_ESTABLISHED || new_status == STATUS_DISCONNECTED {
        refresh_channel();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_onTalkStatusChangeEvent(sch: u64, status: c_int, is_received_whisper: c_int, client_id: u16) {
    let talking = status == STATUS_TALKING;
    let name = if talking { nickname(sch, client_id) } else { String::new() };
    {
        let mut s = lock(&STATE);
        if s.sch != sch {
            return;
        }
        if talking {
            s.talking.insert(client_id, (is_received_whisper != 0, name));
        } else {
            s.talking.remove(&client_id);
        }
    }
    changed();
}

fn on_move(sch: u64, client_id: u16, visibility: c_int) {
    {
        let mut s = lock(&STATE);
        if s.sch != sch {
            return;
        }
        if visibility == LEAVE_VISIBILITY {
            s.talking.remove(&client_id); // clids get recycled
        }
    }
    refresh_channel();
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
        if s.sch != sch {
            return;
        }
        if let Some(t) = s.talking.get_mut(&client_id) {
            t.1 = name.clone();
        }
        if let Some(c) = s.channel.iter_mut().find(|(id, _)| *id == client_id) {
            c.1 = name;
        }
    }
    changed();
}
