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
    rsd::RsdHandshake,
    services::dvt::xctest::{TestConfig, WdaRunHandle, XCUITestService},
    services::installation_proxy::InstallationProxyClient,
    tcp::handle::AdapterHandle,
};
use tokio::{runtime::Runtime, sync::Mutex, task::JoinHandle};

// MARK: - State

struct Session {
    pairing_file: PairingFile,
    host: std::net::IpAddr,
    adapter: AdapterHandle,
    handshake: RsdHandshake,
    heartbeat: JoinHandle<()>,
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
    fn provider(&self) -> TcpProvider {
        TcpProvider {
            addr: self.host,
            scope_id: None,
            pairing_file: self.pairing_file.clone(),
            label: "TapFlow".to_string(),
        }
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

/// Connects to the device at `host` (10.7.0.1 with LocalDevVPN) with the pairing file at `pairing_path`: a
/// heartbeat, then the CoreDevice tunnel and its service list (RSD).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tf_connect(pairing_path: *const c_char, host: *const c_char) -> *mut c_char {
    let args = unsafe { (string(pairing_path), string(host)) };
    result(runtime().block_on(async move {
        let (path, host) = (args.0?, args.1?);
        let pairing_file = PairingFile::read_from_file(&path).map_err(|e| format!("pairing file: {e:?}"))?;
        let host: std::net::IpAddr = host.parse().map_err(|e| format!("host: {e}"))?;
        disconnect().await;

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
        log(&format!("tunnel up: {} services", handshake.services.len()));

        *session().lock().await = Some(Session { pairing_file, host, adapter, handshake, heartbeat, hid: None, wda: None });
        Ok(())
    }))
}

async fn disconnect() {
    if let Some(mut old) = session().lock().await.take() {
        if let Some(hid) = old.hid.take() {
            end_hid(hid).await;
        }
        if let Some(wda) = old.wda.take() {
            wda.abort();
        }
        old.heartbeat.abort();
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

/// iOS version, Developer Mode and the mounted images.
#[unsafe(no_mangle)]
pub extern "C" fn tf_device_status(out: *mut *mut c_char) -> *mut c_char {
    result_with(
        runtime().block_on(async {
            with_session!(s, {
                let provider = s.provider();
                let mut lockdown = LockdownClient::connect(&provider).await.map_err(|e| format!("lockdown: {e:?}"))?;
                lockdown.start_session(&s.pairing_file).await.map_err(|e| format!("lockdown session: {e:?}"))?;
                let version = lockdown
                    .get_value(Some("ProductVersion"), None)
                    .await
                    .ok()
                    .and_then(|v| v.as_string().map(str::to_owned))
                    .unwrap_or_else(|| "?".into());
                let model = lockdown
                    .get_value(Some("ProductType"), None)
                    .await
                    .ok()
                    .and_then(|v| v.as_string().map(str::to_owned))
                    .unwrap_or_else(|| "?".into());
                let mut mounter = ImageMounter::connect(&provider).await.map_err(|e| format!("image mounter: {e:?}"))?;
                let developer_mode = mounter.query_developer_mode_status().await.map_err(|e| format!("{e:?}"))?;
                let images = mounter.copy_devices().await.map_err(|e| format!("{e:?}"))?;
                Ok(format!("iOS {version} ({model})\nDeveloper Mode: {developer_mode}\nMounted images: {}", images.len()))
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
            let provider = s.provider();
            let mut lockdown = LockdownClient::connect(&provider).await.map_err(|e| format!("lockdown: {e:?}"))?;
            lockdown.start_session(&s.pairing_file).await.map_err(|e| format!("lockdown session: {e:?}"))?;
            let chip_id = lockdown
                .get_value(Some("UniqueChipID"), None)
                .await
                .map_err(|e| format!("UniqueChipID: {e:?}"))?
                .as_unsigned_integer()
                .ok_or("UniqueChipID is not a number")?;
            let mut mounter = ImageMounter::connect(&provider).await.map_err(|e| format!("image mounter: {e:?}"))?;
            log("mounting the developer disk image (asks Apple to sign it)…");
            mounter
                .mount_personalized(&provider, image, trust_cache, &manifest, None, chip_id)
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
                let provider = s.provider();
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
                let provider = s.provider();
                let mut proxy = InstallationProxyClient::connect(&provider).await.map_err(|e| format!("installation proxy: {e:?}"))?;
                let config = TestConfig::from_installation_proxy(&mut proxy, &bundle_id, None)
                    .await
                    .map_err(|e| format!("runner {bundle_id}: {e:?}"))?;
                log(&format!("launching {bundle_id}…"));
                let started = Instant::now();
                let service = XCUITestService::new(Arc::new(s.provider()) as Arc<dyn IdeviceProvider>);
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
