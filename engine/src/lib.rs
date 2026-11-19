//! TapFlow's engine (Phase 0 spike). The app connects to this iPhone's *own* developer services through a loopback
//! VPN (LocalDevVPN, 10.7.0.1) and a pairing file, as StikDebug does, then taps in other apps in one of two ways:
//!
//! - **HID**: CoreDevice's HID service (`dtuhidd`, in the developer disk image), the one Xcode's Device Hub uses.
//!   No extra app. Needs a screen media stream to be open while touches are sent.
//! - **XCTest**: launches a WebDriverAgent runner through testmanagerd; the app then talks to it on 127.0.0.1.
//!
//! Every call blocks until done (the app calls from a background queue) and returns NULL on success or an error
//! message to free with `tf_free`. Built on `idevice` (MIT, https://github.com/jkcoxson/idevice).

mod stream;

use std::ffi::{CStr, CString, c_char};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};

use idevice::{
    IdeviceService, ReadWrite, RsdService,
    core_device::{DisplayServiceClient, hid::UniversalHidServiceClient},
    core_device_proxy::CoreDeviceProxy,
    heartbeat::HeartbeatClient,
    lockdown::LockdownClient,
    mobile_image_mounter::ImageMounter,
    pairing_file::PairingFile,
    provider::{IdeviceProvider, TcpProvider},
    remote_pairing::{RemotePairingClient, RpPairingFile, RpPairingSocket, connect_tls_psk_tunnel_native},
    rsd::RsdHandshake,
    services::dvt::xctest::{TestConfig, WdaRunHandle, XCUITestService},
    services::installation_proxy::InstallationProxyClient,
    tcp::{adapter::Adapter, handle::AdapterHandle},
};
use tokio::{runtime::Runtime, sync::Mutex, task::JoinHandle};

// MARK: - State

struct Session {
    /// With a lockdown pairing file (iloader's "Lockdown"); `None` with an RPPairing file (iOS 26.4+), which
    /// gives the tunnel only: enough for HID and the disk image, not for the XCTest engine.
    lockdown: Option<(PairingFile, std::net::IpAddr)>,
    adapter: AdapterHandle,
    handshake: RsdHandshake,
    heartbeat: Option<JoinHandle<()>>,
    hid: Option<HidSession>,
    wda: Option<WdaRunHandle>,
}

struct HidSession {
    client: DisplayServiceClient<Box<dyn ReadWrite>>,
    hid: UniversalHidServiceClient<Box<dyn ReadWrite>>,
    /// Read and discard the device's RTP, so a long run doesn't pile it up in memory (and the ports stay bound).
    drains: Vec<JoinHandle<()>>,
}

impl Session {
    fn provider(&self) -> Result<TcpProvider, String> {
        let (pairing_file, host) = self.lockdown.as_ref().ok_or("this needs a Lockdown pairing file (not RPPairing)")?;
        Ok(TcpProvider { addr: *host, scope_id: None, pairing_file: pairing_file.clone(), label: "TapFlow".to_string() })
    }
}

fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("tokio runtime")
    })
}

fn session() -> &'static Mutex<Option<Session>> {
    static SESSION: OnceLock<Mutex<Option<Session>>> = OnceLock::new();
    SESSION.get_or_init(|| Mutex::new(None))
}

// MARK: - Logging

type LogCallback = extern "C" fn(*const c_char);
static LOG: StdMutex<Option<LogCallback>> = StdMutex::new(None);

pub(crate) fn log(message: &str) {
    if let Ok(guard) = LOG.lock() {
        if let Some(callback) = *guard {
            if let Ok(text) = CString::new(message.replace('\0', "")) {
                callback(text.as_ptr());
            }
        }
    }
}

/// Lines of progress go to `callback` (from any thread).
#[unsafe(no_mangle)]
pub extern "C" fn tf_set_log(callback: Option<LogCallback>) {
    if let Ok(mut guard) = LOG.lock() {
        *guard = callback;
    }
}

// MARK: - C helpers

fn result(r: Result<(), String>) -> *mut c_char {
    match r {
        Ok(()) => std::ptr::null_mut(),
        Err(e) => {
            log(&format!("error: {e}"));
            CString::new(e.replace('\0', "")).map(CString::into_raw).unwrap_or(std::ptr::null_mut())
        }
    }
}

fn result_with(r: Result<String, String>, out: *mut *mut c_char) -> *mut c_char {
    match r {
        Ok(text) => {
            if !out.is_null() {
                let text = CString::new(text.replace('\0', "")).unwrap_or_default();
                unsafe { *out = text.into_raw() };
            }
            std::ptr::null_mut()
        }
        Err(e) => result(Err(e)),
    }
}

unsafe fn string(ptr: *const c_char) -> Result<String, String> {
    if ptr.is_null() {
        return Err("missing argument".into());
    }
    unsafe { CStr::from_ptr(ptr) }.to_str().map(str::to_owned).map_err(|e| e.to_string())
}

/// Frees a string returned by this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tf_free(ptr: *mut c_char) {
    if !ptr.is_null() {
        drop(unsafe { CString::from_raw(ptr) });
    }
}

macro_rules! with_session {
    ($session:ident, $body:expr) => {{
        let mut guard = session().lock().await;
        let Some($session) = guard.as_mut() else {
            return Err("not connected".to_string());
        };
        $body
    }};
}

// MARK: - Connecting

/// Connects to the device at `host` (10.7.0.1 with LocalDevVPN) with the pairing file at `pairing_path`, and
/// opens the CoreDevice tunnel and its service list (RSD). A Lockdown pairing file goes through lockdown (with a
/// heartbeat); an RPPairing file (iOS 26.4+) through remote pairing on `rp_port` (49152 unless Bonjour says
/// otherwise).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tf_connect(pairing_path: *const c_char, host: *const c_char, rp_port: u16) -> *mut c_char {
    let args = unsafe { (string(pairing_path), string(host)) };
    result(runtime().block_on(async move {
        let (path, host) = (args.0?, args.1?);
        let bytes = std::fs::read(&path).map_err(|e| format!("pairing file: {e}"))?;
        let host: std::net::IpAddr = host.parse().map_err(|e| format!("host: {e}"))?;
        disconnect().await;

        let pairing_file = match PairingFile::from_bytes(&bytes) {
            Ok(file) => file,
            Err(lockdown_error) => {
                let mut rp_file = RpPairingFile::from_bytes(&bytes)
                    .map_err(|e| format!("neither a Lockdown ({lockdown_error:?}) nor an RPPairing file ({e:?})"))?;
                let (adapter, handshake) = connect_rppairing(host, rp_port, &mut rp_file).await?;
                *session().lock().await =
                    Some(Session { lockdown: None, adapter, handshake, heartbeat: None, hid: None, wda: None });
                return Ok(());
            }
        };

        let provider = TcpProvider { addr: host, scope_id: None, pairing_file: pairing_file.clone(), label: "TapFlow".into() };
        log(&format!("connecting to {host}…"));
        // The device drops a network lockdown connection without a heartbeat.
        let mut heartbeat_client = HeartbeatClient::connect(&provider).await.map_err(|e| format!("heartbeat: {e:?}"))?;
        let heartbeat = tokio::spawn(async move {
            let mut interval = 15;
            loop {
                match heartbeat_client.get_marco(interval).await {
                    Ok(next) => interval = next + 5,
                    Err(e) => {
                        log(&format!("heartbeat stopped: {e:?}"));
                        return;
                    }
                }
                if let Err(e) = heartbeat_client.send_polo().await {
                    log(&format!("heartbeat stopped: {e:?}"));
                    return;
                }
            }
        });
        log("heartbeat running");

        let proxy = CoreDeviceProxy::connect(&provider).await.map_err(|e| format!("CoreDeviceProxy: {e:?}"))?;
        let rsd_port = proxy.tunnel_info().server_rsd_port;
        let adapter = proxy.create_software_tunnel().map_err(|e| format!("software tunnel: {e:?}"))?;
        let mut adapter = adapter.to_async_handle();
        let stream = adapter.connect(rsd_port).await.map_err(|e| format!("RSD connect: {e:?}"))?;
        let handshake = RsdHandshake::new(stream).await.map_err(|e| format!("RSD handshake: {e:?}"))?;
        log(&format!("tunnel up (Lockdown): {} services", handshake.services.len()));

        *session().lock().await = Some(Session {
            lockdown: Some((pairing_file, host)),
            adapter,
            handshake,
            heartbeat: Some(heartbeat),
            hid: None,
            wda: None,
        });
        Ok(())
    }))
}

/// The tunnel through remote pairing, as idevice's FFI `tunnel_create_rppairing` (StikDebug's path since iOS 26.4).
async fn connect_rppairing(
    host: std::net::IpAddr,
    port: u16,
    pairing_file: &mut RpPairingFile,
) -> Result<(AdapterHandle, RsdHandshake), String> {
    log(&format!("connecting to {host}:{port} (RPPairing)…"));
    let stream = tokio::net::TcpStream::connect((host, port)).await.map_err(|e| format!("connect {host}:{port}: {e}"))?;
    let mut client = RemotePairingClient::new(RpPairingSocket::new(stream), "TapFlow");
    client
        .connect(pairing_file, || async { String::new() })
        .await
        .map_err(|e| format!("pair verify: {e:?}"))?;
    let tunnel_port = client.create_tcp_listener().await.map_err(|e| format!("tunnel listener: {e:?}"))?;
    let tunnel_stream =
        tokio::net::TcpStream::connect((host, tunnel_port)).await.map_err(|e| format!("tunnel connect: {e}"))?;
    let tunnel = connect_tls_psk_tunnel_native(tunnel_stream, client.encryption_key())
        .await
        .map_err(|e| format!("TLS tunnel: {e:?}"))?;
    let client_ip: std::net::IpAddr = tunnel.info.client_address.parse().map_err(|e| format!("client address: {e}"))?;
    let server_ip: std::net::IpAddr = tunnel.info.server_address.parse().map_err(|e| format!("server address: {e}"))?;
    let mtu = tunnel.info.mtu as usize;
    let rsd_port = tunnel.info.server_rsd_port;
    let mut adapter = Adapter::new(Box::new(tunnel.into_inner()), client_ip, server_ip);
    adapter.set_mss(mtu.saturating_sub(60));
    let mut adapter = adapter.to_async_handle();
    let stream = adapter.connect(rsd_port).await.map_err(|e| format!("RSD connect: {e:?}"))?;
    let handshake = RsdHandshake::new(stream).await.map_err(|e| format!("RSD handshake: {e:?}"))?;
    log(&format!("tunnel up (RPPairing): {} services", handshake.services.len()));
    Ok((adapter, handshake))
}

async fn disconnect() {
    if let Some(mut old) = session().lock().await.take() {
        if let Some(hid) = old.hid.take() {
            end_hid(hid).await;
        }
        if let Some(wda) = old.wda.take() {
            wda.abort();
        }
        if let Some(heartbeat) = old.heartbeat.take() {
            heartbeat.abort();
        }
        let _ = old.adapter.close().await;
        log("disconnected");
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn tf_disconnect() {
    runtime().block_on(disconnect());
}

/// The service names over the tunnel, one per line, and which engines they allow.
#[unsafe(no_mangle)]
pub extern "C" fn tf_services(out: *mut *mut c_char) -> *mut c_char {
    result_with(
        runtime().block_on(async {
            with_session!(s, {
                let mut names: Vec<&String> = s.handshake.services.keys().collect();
                names.sort();
                let has = |name: &str| s.handshake.services.contains_key(name);
                let mut text = format!(
                    "HID (dtuhidd): {}\nscreen stream: {}\ntestmanagerd: {}\n\n",
                    has("com.apple.coredevice.hid.universalhidservice"),
                    has("com.apple.coredevice.displayservice") || names.iter().any(|n| n.contains("displayservice")),
                    names.iter().any(|n| n.contains("testmanagerd")),
                );
                for name in names {
                    text.push_str(name);
                    text.push('\n');
                }
                Ok(text)
            })
        }),
        out,
    )
}

// MARK: - Developer disk image

/// A device property from the RSD handshake (e.g. `OSVersion`, `ProductType`, `UniqueChipID`).
fn property(s: &Session, key: &str) -> Option<plist::Value> {
    s.handshake.properties.get(key).cloned()
}

/// The chip ID the disk image is personalized for: from RSD, else from lockdown.
async fn unique_chip_id(s: &Session) -> Result<u64, String> {
    if let Some(id) = property(s, "UniqueChipID").and_then(|v| v.as_unsigned_integer()) {
        return Ok(id);
    }
    let provider = s.provider()?;
    let (pairing_file, _) = s.lockdown.as_ref().ok_or("no UniqueChipID")?;
    let mut lockdown = LockdownClient::connect(&provider).await.map_err(|e| format!("lockdown: {e:?}"))?;
    lockdown.start_session(pairing_file).await.map_err(|e| format!("lockdown session: {e:?}"))?;
    lockdown
        .get_value(Some("UniqueChipID"), None)
        .await
        .map_err(|e| format!("UniqueChipID: {e:?}"))?
        .as_unsigned_integer()
        .ok_or_else(|| "UniqueChipID is not a number".to_string())
}

/// iOS version, how we're connected, Developer Mode and the mounted images.
#[unsafe(no_mangle)]
pub extern "C" fn tf_device_status(out: *mut *mut c_char) -> *mut c_char {
    result_with(
        runtime().block_on(async {
            with_session!(s, {
                let text = |key: &str| property(s, key).and_then(|v| v.as_string().map(str::to_owned)).unwrap_or_else(|| "?".into());
                let header = format!(
                    "iOS {} ({}), connected with {}",
                    text("OSVersion"),
                    text("ProductType"),
                    if s.lockdown.is_some() { "Lockdown" } else { "RPPairing" }
                );
                let mut mounter = ImageMounter::connect_rsd(&mut s.adapter, &mut s.handshake)
                    .await
                    .map_err(|e| format!("image mounter: {e:?}"))?;
                let developer_mode = mounter.query_developer_mode_status().await.map_err(|e| format!("{e:?}"))?;
                let images = mounter.copy_devices().await.map_err(|e| format!("{e:?}"))?;
                Ok(format!("{header}\nDeveloper Mode: {developer_mode}\nMounted images: {}", images.len()))
            })
        }),
        out,
    )
}

/// Mounts the personalized developer disk image (Image.dmg, BuildManifest.plist, Image.dmg.trustcache).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tf_mount_ddi(image: *const c_char, manifest: *const c_char, trust_cache: *const c_char) -> *mut c_char {
    let args = unsafe { (string(image), string(manifest), string(trust_cache)) };
    result(runtime().block_on(async move {
        let image = std::fs::read(args.0?).map_err(|e| format!("Image.dmg: {e}"))?;
        let manifest = std::fs::read(args.1?).map_err(|e| format!("BuildManifest.plist: {e}"))?;
        let trust_cache = std::fs::read(args.2?).map_err(|e| format!("trust cache: {e}"))?;
        with_session!(s, {
            let chip_id = unique_chip_id(s).await?;
            let mut mounter = ImageMounter::connect_rsd(&mut s.adapter, &mut s.handshake)
                .await
                .map_err(|e| format!("image mounter: {e:?}"))?;
            log("mounting the developer disk image (asks Apple to sign it)…");
            mounter
                .mount_personalized_rsd(&mut s.adapter, &mut s.handshake, image, trust_cache, &manifest, None, chip_id)
                .await
                .map_err(|e| format!("mount: {e:?}"))?;
            log("developer disk image mounted");
            Ok(())
        })
    }))
}

// MARK: - HID engine

async fn end_hid(mut hid: HidSession) {
    for drain in hid.drains.drain(..) {
        drain.abort();
    }
    if let Err(e) = hid.client.stop_media_stream().await {
        log(&format!("stop_media_stream: {e:?}"));
    }
}

/// Opens the screen media stream (the HID gate) and the HID service.
#[unsafe(no_mangle)]
pub extern "C" fn tf_hid_begin() -> *mut c_char {
    result(runtime().block_on(async {
        with_session!(s, {
            if let Some(old) = s.hid.take() {
                end_hid(old).await;
            }
            let started = stream::start_screen_media_stream(&mut s.adapter, &mut s.handshake, 1).await?;
            let stream::ScreenMediaStream { client, audio_udp, video_udp } = started;
            let drains = vec![
                tokio::spawn(async move { while audio_udp.recv().await.is_ok() {} }),
                tokio::spawn(async move { while video_udp.recv().await.is_ok() {} }),
            ];
            // backboardd needs a moment to match the HID surfaces against the authenticated stream.
            tokio::time::sleep(Duration::from_millis(300)).await;
            let hid = UniversalHidServiceClient::connect_rsd(&mut s.adapter, &mut s.handshake)
                .await
                .map_err(|e| format!("no hid.universalhidservice: {e:?}"))?;
            s.hid = Some(HidSession { client, hid, drains });
            log("HID ready");
            Ok(())
        })
    }))
}

/// A tap at (`x`, `y`) in 0–65535 screen units (32768 = centre).
#[unsafe(no_mangle)]
pub extern "C" fn tf_hid_tap(x: u16, y: u16) -> *mut c_char {
    result(runtime().block_on(async {
        with_session!(s, {
            let hid = s.hid.as_mut().ok_or("HID not started")?;
            hid.hid.tap(x, y).await.map_err(|e| format!("tap: {e:?}"))
        })
    }))
}

/// A drag from (`x1`, `y1`) to (`x2`, `y2`) in `steps` samples `delay_ms` apart.
#[unsafe(no_mangle)]
pub extern "C" fn tf_hid_drag(x1: u16, y1: u16, x2: u16, y2: u16, steps: u32, delay_ms: u64) -> *mut c_char {
    result(runtime().block_on(async {
        with_session!(s, {
            let hid = s.hid.as_mut().ok_or("HID not started")?;
            hid.hid.drag(x1, y1, x2, y2, steps, delay_ms).await.map_err(|e| format!("drag: {e:?}"))
        })
    }))
}

#[unsafe(no_mangle)]
pub extern "C" fn tf_hid_end() -> *mut c_char {
    result(runtime().block_on(async {
        with_session!(s, {
            if let Some(hid) = s.hid.take() {
                end_hid(hid).await;
            }
            Ok(())
        })
    }))
}

// MARK: - XCTest engine

/// Installed apps whose bundle identifier contains "xctrunner" (WebDriverAgent runners), one per line.
#[unsafe(no_mangle)]
pub extern "C" fn tf_find_runners(out: *mut *mut c_char) -> *mut c_char {
    result_with(
        runtime().block_on(async {
            with_session!(s, {
                let provider = s.provider()?;
                let mut proxy = InstallationProxyClient::connect(&provider).await.map_err(|e| format!("installation proxy: {e:?}"))?;
                let apps = proxy.get_apps(Some("User"), None).await.map_err(|e| format!("{e:?}"))?;
                let mut ids: Vec<String> = apps.into_keys().filter(|id| id.to_lowercase().contains("xctrunner")).collect();
                ids.sort();
                Ok(ids.join("\n"))
            })
        }),
        out,
    )
}

/// Launches the WebDriverAgent runner `bundle_id` and waits until it answers; `out` gets its HTTP port on the
/// device (reach it at http://127.0.0.1:<port>).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tf_wda_start(bundle_id: *const c_char, out: *mut *mut c_char) -> *mut c_char {
    let bundle_id = unsafe { string(bundle_id) };
    result_with(
        runtime().block_on(async move {
            let bundle_id = bundle_id?;
            with_session!(s, {
                if let Some(old) = s.wda.take() {
                    old.abort();
                }
                let provider = s.provider()?;
                let mut proxy = InstallationProxyClient::connect(&provider).await.map_err(|e| format!("installation proxy: {e:?}"))?;
                let config = TestConfig::from_installation_proxy(&mut proxy, &bundle_id, None)
                    .await
                    .map_err(|e| format!("runner {bundle_id}: {e:?}"))?;
                log(&format!("launching {bundle_id}…"));
                let started = Instant::now();
                let service = XCUITestService::new(Arc::new(s.provider()?) as Arc<dyn IdeviceProvider>);
                let handle = service
                    .run_until_wda_ready(config, Duration::from_secs(60))
                    .await
                    .map_err(|e| format!("XCTest: {e:?}"))?;
                let port = handle.ports().http;
                log(&format!("WDA ready on port {port} after {:.1} s", started.elapsed().as_secs_f64()));
                s.wda = Some(handle);
                Ok(port.to_string())
            })
        }),
        out,
    )
}

#[unsafe(no_mangle)]
pub extern "C" fn tf_wda_stop() {
    runtime().block_on(async {
        if let Some(s) = session().lock().await.as_mut() {
            if let Some(wda) = s.wda.take() {
                wda.abort();
                log("WDA stopped");
            }
        }
    });
}
