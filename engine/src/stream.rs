// Adapted from idevice-tools `coredevice_stream.rs` (v0.1.68) by Jackson Coxson, MIT License,
// https://github.com/jkcoxson/idevice. Changed: logging goes to TapFlow's log; no answer dump.
//
// CoreDevice's HID daemon (dtuhidd) drops synthetic touches unless a displayservice media stream is active.
// The stream only has to exist while touches are sent; its RTP payload is read and discarded (see lib.rs).

use idevice::{
    ReadWrite, RsdService,
    core_device::{
        CallInfoBlob, DisplayServiceClient, build_screen_audio_offer, build_screen_video_offer,
        build_start_audio_parameters, build_start_video_parameters,
    },
    rsd::RsdHandshake,
    tcp::handle::{AdapterHandle, UdpSocketHandle},
};

use crate::log;

/// What we (the controller) support, as Device Hub sends for screen sharing.
const CLIENT_SUPPORTED_FEATURES: u64 = 140;

/// An active screen media stream and the UDP sockets the device sends RTP to.
pub struct ScreenMediaStream {
    pub client: DisplayServiceClient<Box<dyn ReadWrite>>,
    pub audio_udp: UdpSocketHandle,
    pub video_udp: UdpSocketHandle,
}

/// Connects the displayservice and starts the audio and video screen-sharing session, as Device Hub does.
pub async fn start_screen_media_stream(
    adapter: &mut AdapterHandle,
    handshake: &mut RsdHandshake,
    display_id: i64,
) -> Result<ScreenMediaStream, String> {
    let mut client = DisplayServiceClient::connect_rsd(adapter, handshake)
        .await
        .map_err(|e| format!("no display service: {e:?}"))?;

    let audio_udp = adapter
        .bind_udp(0)
        .await
        .map_err(|e| format!("bind_udp(audio) failed: {e:?}"))?;
    let video_udp = adapter
        .bind_udp(0)
        .await
        .map_err(|e| format!("bind_udp(video) failed: {e:?}"))?;
    let receiver_ip = adapter.host_ip().to_string();
    let audio_receiver_port = audio_udp.local_port();
    let receiver_port = video_udp.local_port();
    let sender_ip = adapter.peer_ip().to_string();
    log(&format!("video receiver = {receiver_ip}:{receiver_port}, sender = {sender_ip}"));

    let call_info = CallInfoBlob {
        call_id: 0,
        client_version: 1,
        device_type: "Mac17,7".into(),
        framework_version: "2205.3.1".into(),
        os_version: "25F71".into(),
        device_name: None,
        audio_device_uid: None,
    };
    let client_session_id = uuid::Uuid::new_v4();

    let audio_call_id = uuid::Uuid::new_v4().to_string().to_uppercase();
    let audio_offer = build_screen_audio_offer(&audio_call_id, &call_info)
        .map_err(|e| format!("audio offer build failed: {e:?}"))?;
    let audio_params = build_start_audio_parameters(
        &receiver_ip,
        audio_receiver_port,
        &sender_ip,
        50000,
        audio_offer,
        CLIENT_SUPPORTED_FEATURES,
        client_session_id,
    );
    client
        .start_media_stream(audio_params)
        .await
        .map_err(|e| format!("audio startMediaStream failed: {e:?}"))?;
    log(&format!("audio stream started (session {client_session_id})"));

    let call_id = uuid::Uuid::new_v4().to_string().to_uppercase();
    let our_ssrc = uuid::Uuid::new_v4().as_u128() as u32;
    let offer = build_screen_video_offer(&call_id, &call_info, our_ssrc)
        .map_err(|e| format!("video offer build failed: {e:?}"))?;
    let params = build_start_video_parameters(
        &receiver_ip,
        receiver_port,
        &sender_ip,
        50001,
        offer,
        CLIENT_SUPPORTED_FEATURES,
        display_id,
        client_session_id,
    );
    client
        .start_media_stream(params)
        .await
        .map_err(|e| format!("video startMediaStream failed: {e:?}"))?;
    log(&format!("video stream started (session {client_session_id})"));

    Ok(ScreenMediaStream {
        client,
        audio_udp,
        video_udp,
    })
}
