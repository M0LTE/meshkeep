// SPDX-License-Identifier: AGPL-3.0-or-later
//! MeshCore companion protocol: framing, codes and the few frame layouts
//! meshkeep needs to understand. Source of truth is the companion firmware
//! (`examples/companion_radio/MyMesh.cpp`, `src/helpers/wifi/SerialWifiInterface.cpp`,
//! `src/helpers/ArduinoSerialInterface.cpp`).

/// Largest frame the firmware accepts or emits (`MAX_FRAME_SIZE`).
pub const MAX_FRAME_SIZE: usize = 176;
/// Frame marker, app to radio.
pub const FRAME_TO_RADIO: u8 = b'<';
/// Frame marker, radio to app.
pub const FRAME_FROM_RADIO: u8 = b'>';

pub const PUB_KEY_SIZE: usize = 32;
pub const MAX_PATH_SIZE: usize = 64;
/// `MAX_TEXT_LEN` in BaseChatMesh.h.
pub const MAX_TEXT_LEN: usize = 160;

pub mod cmd {
    pub const APP_START: u8 = 1;
    pub const SEND_TXT_MSG: u8 = 2;
    pub const SEND_CHANNEL_TXT_MSG: u8 = 3;
    pub const GET_CONTACTS: u8 = 4;
    pub const GET_DEVICE_TIME: u8 = 5;
    pub const SET_DEVICE_TIME: u8 = 6;
    pub const SEND_SELF_ADVERT: u8 = 7;
    pub const SET_ADVERT_NAME: u8 = 8;
    pub const ADD_UPDATE_CONTACT: u8 = 9;
    pub const SYNC_NEXT_MESSAGE: u8 = 10;
    pub const SET_RADIO_PARAMS: u8 = 11;
    pub const SET_RADIO_TX_POWER: u8 = 12;
    pub const RESET_PATH: u8 = 13;
    pub const SET_ADVERT_LATLON: u8 = 14;
    pub const REMOVE_CONTACT: u8 = 15;
    pub const SHARE_CONTACT: u8 = 16;
    pub const EXPORT_CONTACT: u8 = 17;
    pub const IMPORT_CONTACT: u8 = 18;
    pub const REBOOT: u8 = 19;
    pub const GET_BATT_AND_STORAGE: u8 = 20;
    pub const SET_TUNING_PARAMS: u8 = 21;
    pub const DEVICE_QUERY: u8 = 22;
    pub const EXPORT_PRIVATE_KEY: u8 = 23;
    pub const IMPORT_PRIVATE_KEY: u8 = 24;
    pub const SEND_RAW_DATA: u8 = 25;
    pub const SEND_LOGIN: u8 = 26;
    pub const SEND_STATUS_REQ: u8 = 27;
    pub const HAS_CONNECTION: u8 = 28;
    pub const LOGOUT: u8 = 29;
    pub const GET_CONTACT_BY_KEY: u8 = 30;
    pub const GET_CHANNEL: u8 = 31;
    pub const SET_CHANNEL: u8 = 32;
    pub const SIGN_START: u8 = 33;
    pub const SIGN_DATA: u8 = 34;
    pub const SIGN_FINISH: u8 = 35;
    pub const SEND_TRACE_PATH: u8 = 36;
    pub const SET_DEVICE_PIN: u8 = 37;
    pub const SET_OTHER_PARAMS: u8 = 38;
    pub const SEND_TELEMETRY_REQ: u8 = 39;
    pub const GET_CUSTOM_VARS: u8 = 40;
    pub const SET_CUSTOM_VAR: u8 = 41;
    pub const GET_ADVERT_PATH: u8 = 42;
    pub const GET_TUNING_PARAMS: u8 = 43;
    pub const SEND_BINARY_REQ: u8 = 50;
    pub const FACTORY_RESET: u8 = 51;
    pub const SEND_PATH_DISCOVERY_REQ: u8 = 52;
    pub const SET_FLOOD_SCOPE_KEY: u8 = 54;
    pub const SEND_CONTROL_DATA: u8 = 55;
    pub const GET_STATS: u8 = 56;
    pub const SEND_ANON_REQ: u8 = 57;
    pub const SET_AUTOADD_CONFIG: u8 = 58;
    pub const GET_AUTOADD_CONFIG: u8 = 59;
    pub const GET_ALLOWED_REPEAT_FREQ: u8 = 60;
    pub const SET_PATH_HASH_MODE: u8 = 61;
    pub const SEND_CHANNEL_DATA: u8 = 62;
    pub const SET_DEFAULT_FLOOD_SCOPE: u8 = 63;
    pub const GET_DEFAULT_FLOOD_SCOPE: u8 = 64;
    pub const SEND_RAW_PACKET: u8 = 65;
    pub const RUN_CLI_COMMAND: u8 = 66;
}

pub mod resp {
    pub const OK: u8 = 0;
    pub const ERR: u8 = 1;
    pub const CONTACTS_START: u8 = 2;
    pub const CONTACT: u8 = 3;
    pub const END_OF_CONTACTS: u8 = 4;
    pub const SELF_INFO: u8 = 5;
    pub const SENT: u8 = 6;
    pub const CONTACT_MSG_RECV: u8 = 7;
    pub const CHANNEL_MSG_RECV: u8 = 8;
    pub const CURR_TIME: u8 = 9;
    pub const NO_MORE_MESSAGES: u8 = 10;
    pub const EXPORT_CONTACT: u8 = 11;
    pub const BATT_AND_STORAGE: u8 = 12;
    pub const DEVICE_INFO: u8 = 13;
    pub const PRIVATE_KEY: u8 = 14;
    pub const DISABLED: u8 = 15;
    pub const CONTACT_MSG_RECV_V3: u8 = 16;
    pub const CHANNEL_MSG_RECV_V3: u8 = 17;
    pub const CHANNEL_INFO: u8 = 18;
    pub const SIGN_START: u8 = 19;
    pub const SIGNATURE: u8 = 20;
    pub const CUSTOM_VARS: u8 = 21;
    pub const ADVERT_PATH: u8 = 22;
    pub const TUNING_PARAMS: u8 = 23;
    pub const STATS: u8 = 24;
    pub const AUTOADD_CONFIG: u8 = 25;
    pub const ALLOWED_REPEAT_FREQ: u8 = 26;
    pub const CHANNEL_DATA_RECV: u8 = 27;
    pub const DEFAULT_FLOOD_SCOPE: u8 = 28;
    pub const CLI_REPLY: u8 = 29;
}

pub mod push {
    pub const ADVERT: u8 = 0x80;
    pub const PATH_UPDATED: u8 = 0x81;
    pub const SEND_CONFIRMED: u8 = 0x82;
    pub const MSG_WAITING: u8 = 0x83;
    pub const RAW_DATA: u8 = 0x84;
    pub const LOGIN_SUCCESS: u8 = 0x85;
    pub const LOGIN_FAIL: u8 = 0x86;
    pub const STATUS_RESPONSE: u8 = 0x87;
    pub const LOG_RX_DATA: u8 = 0x88;
    pub const TRACE_DATA: u8 = 0x89;
    pub const NEW_ADVERT: u8 = 0x8A;
    pub const TELEMETRY_RESPONSE: u8 = 0x8B;
    pub const BINARY_RESPONSE: u8 = 0x8C;
    pub const PATH_DISCOVERY_RESPONSE: u8 = 0x8D;
    pub const CONTROL_DATA: u8 = 0x8E;
    pub const CONTACT_DELETED: u8 = 0x8F;
    pub const CONTACTS_FULL: u8 = 0x90;
}

pub mod err {
    pub const UNSUPPORTED_CMD: u8 = 1;
    pub const NOT_FOUND: u8 = 2;
    pub const TABLE_FULL: u8 = 3;
    pub const BAD_STATE: u8 = 4;
    pub const FILE_IO_ERROR: u8 = 5;
    pub const ILLEGAL_ARG: u8 = 6;
}

pub mod txt {
    pub const PLAIN: u8 = 0;
    pub const CLI_DATA: u8 = 1;
    pub const SIGNED_PLAIN: u8 = 2;
    pub const CLI_COMMAND: u8 = 3;
}

pub fn is_push(code: u8) -> bool {
    code >= 0x80
}

/// Wrap a payload in the serial/TCP frame header used in direction `marker`.
pub fn encode_frame(marker: u8, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(payload.len() + 3);
    v.push(marker);
    v.push((payload.len() & 0xff) as u8);
    v.push((payload.len() >> 8) as u8);
    v.extend_from_slice(payload);
    v
}

/// Incremental decoder for `<`/`>` + u16le length framed streams.
///
/// Mirrors the firmware: bytes before a valid marker are skipped, zero length
/// frames are ignored, and frames longer than `max_len` are skipped whole.
pub struct FrameDecoder {
    marker: u8,
    max_len: usize,
    buf: Vec<u8>,
}

impl FrameDecoder {
    pub fn new(marker: u8, max_len: usize) -> Self {
        FrameDecoder {
            marker,
            max_len,
            buf: Vec::new(),
        }
    }

    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Next complete frame payload, if one is buffered.
    pub fn next_frame(&mut self) -> Option<Vec<u8>> {
        loop {
            // Resynchronise on the marker byte.
            match self.buf.iter().position(|&b| b == self.marker) {
                Some(0) => {}
                Some(p) => {
                    self.buf.drain(..p);
                }
                None => {
                    self.buf.clear();
                    return None;
                }
            }
            if self.buf.len() < 3 {
                return None;
            }
            let len = self.buf[1] as usize | ((self.buf[2] as usize) << 8);
            if len == 0 {
                self.buf.drain(..3);
                continue;
            }
            if self.buf.len() < 3 + len {
                if len > self.max_len && len > 4096 {
                    // Implausible header, almost certainly line noise: skip the marker.
                    self.buf.drain(..1);
                    continue;
                }
                return None;
            }
            let frame: Vec<u8> = self.buf[3..3 + len].to_vec();
            self.buf.drain(..3 + len);
            if len > self.max_len {
                continue;
            }
            return Some(frame);
        }
    }
}

/// What a command's response looks like, so the proxy knows when a command is
/// finished and which radio frames belong to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// The firmware sends nothing back (reboot, factory reset).
    Nothing,
    /// One frame with one of these codes, or `RESP_CODE_ERR`.
    One(&'static [u8]),
    /// One frame of any non-push code (unknown or future commands).
    AnyOne,
    /// Contact listing: `CONTACTS_START`, `CONTACT`..., `END_OF_CONTACTS` (or `ERR`).
    Contacts,
    /// Self telemetry: a `PUSH_CODE_TELEMETRY_RESPONSE` frame is the reply.
    SelfTelemetry,
    /// `SYNC_NEXT_MESSAGE`: any non-push frame.
    SyncNext,
}

pub fn expect_for(frame: &[u8]) -> Expect {
    use cmd::*;
    use resp as r;
    let Some(&code) = frame.first() else {
        return Expect::Nothing;
    };
    const OKS: &[u8] = &[r::OK];
    match code {
        APP_START => Expect::One(&[r::SELF_INFO]),
        SEND_TXT_MSG
        | SEND_LOGIN
        | SEND_STATUS_REQ
        | SEND_TRACE_PATH
        | SEND_BINARY_REQ
        | SEND_PATH_DISCOVERY_REQ
        | SEND_ANON_REQ => Expect::One(&[r::SENT]),
        GET_CONTACTS => Expect::Contacts,
        GET_DEVICE_TIME => Expect::One(&[r::CURR_TIME]),
        SYNC_NEXT_MESSAGE => Expect::SyncNext,
        EXPORT_CONTACT => Expect::One(&[r::EXPORT_CONTACT]),
        REBOOT if frame.len() >= 7 && &frame[1..7] == b"reboot" => Expect::Nothing,
        FACTORY_RESET if frame.len() >= 6 && &frame[1..6] == b"reset" => Expect::Nothing,
        GET_BATT_AND_STORAGE => Expect::One(&[r::BATT_AND_STORAGE]),
        DEVICE_QUERY => Expect::One(&[r::DEVICE_INFO]),
        EXPORT_PRIVATE_KEY => Expect::One(&[r::PRIVATE_KEY, r::DISABLED]),
        IMPORT_PRIVATE_KEY => Expect::One(&[r::OK, r::DISABLED]),
        GET_CONTACT_BY_KEY => Expect::One(&[r::CONTACT]),
        GET_CHANNEL => Expect::One(&[r::CHANNEL_INFO]),
        SIGN_START => Expect::One(&[r::SIGN_START]),
        SIGN_FINISH => Expect::One(&[r::SIGNATURE]),
        SEND_TELEMETRY_REQ if frame.len() == 4 => Expect::SelfTelemetry,
        SEND_TELEMETRY_REQ => Expect::One(&[r::SENT]),
        GET_CUSTOM_VARS => Expect::One(&[r::CUSTOM_VARS]),
        GET_ADVERT_PATH => Expect::One(&[r::ADVERT_PATH]),
        GET_TUNING_PARAMS => Expect::One(&[r::TUNING_PARAMS]),
        GET_STATS => Expect::One(&[r::STATS]),
        GET_AUTOADD_CONFIG => Expect::One(&[r::AUTOADD_CONFIG]),
        GET_ALLOWED_REPEAT_FREQ => Expect::One(&[r::ALLOWED_REPEAT_FREQ]),
        GET_DEFAULT_FLOOD_SCOPE => Expect::One(&[r::DEFAULT_FLOOD_SCOPE]),
        RUN_CLI_COMMAND => Expect::One(&[r::CLI_REPLY]),
        SEND_CHANNEL_TXT_MSG
        | SET_DEVICE_TIME
        | SEND_SELF_ADVERT
        | SET_ADVERT_NAME
        | ADD_UPDATE_CONTACT
        | SET_RADIO_PARAMS
        | SET_RADIO_TX_POWER
        | RESET_PATH
        | SET_ADVERT_LATLON
        | REMOVE_CONTACT
        | SHARE_CONTACT
        | IMPORT_CONTACT
        | SET_TUNING_PARAMS
        | SEND_RAW_DATA
        | HAS_CONNECTION
        | LOGOUT
        | SET_CHANNEL
        | SIGN_DATA
        | SET_DEVICE_PIN
        | SET_OTHER_PARAMS
        | SET_CUSTOM_VAR
        | SET_FLOOD_SCOPE_KEY
        | SEND_CONTROL_DATA
        | SET_AUTOADD_CONFIG
        | SET_PATH_HASH_MODE
        | SEND_CHANNEL_DATA
        | SET_DEFAULT_FLOOD_SCOPE
        | SEND_RAW_PACKET => Expect::One(OKS),
        // A malformed reboot/reset falls through to the firmware's final
        // `else`, which answers ERR_CODE_UNSUPPORTED_CMD.
        REBOOT | FACTORY_RESET => Expect::One(&[]),
        _ => Expect::AnyOne,
    }
}

pub fn ok_frame() -> Vec<u8> {
    vec![resp::OK]
}

pub fn err_frame(code: u8) -> Vec<u8> {
    vec![resp::ERR, code]
}

fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn i32le(b: &[u8], at: usize) -> i32 {
    u32le(b, at) as i32
}

/// NUL terminated, fixed width string field.
fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Length of a `RESP_CODE_CONTACT` frame (also `PUSH_CODE_NEW_ADVERT`).
pub const CONTACT_FRAME_LEN: usize = 1 + 32 + 3 + MAX_PATH_SIZE + 32 + 16;

#[derive(Debug, Clone, PartialEq)]
pub struct Contact {
    pub pubkey: [u8; 32],
    pub kind: u8,
    pub flags: u8,
    pub name: String,
    pub last_advert: u32,
    pub lat: i32,
    pub lon: i32,
    pub lastmod: u32,
}

/// Parse a `RESP_CODE_CONTACT` / `PUSH_CODE_NEW_ADVERT` frame.
pub fn parse_contact(f: &[u8]) -> Option<Contact> {
    if f.len() < CONTACT_FRAME_LEN {
        return None;
    }
    let mut pubkey = [0u8; 32];
    pubkey.copy_from_slice(&f[1..33]);
    let name_at = 36 + MAX_PATH_SIZE;
    let ts_at = name_at + 32;
    Some(Contact {
        pubkey,
        kind: f[33],
        flags: f[34],
        name: cstr(&f[name_at..name_at + 32]),
        last_advert: u32le(f, ts_at),
        lat: i32le(f, ts_at + 4),
        lon: i32le(f, ts_at + 8),
        lastmod: u32le(f, ts_at + 12),
    })
}

/// Lastmod field of a contact frame.
pub fn contact_lastmod(f: &[u8]) -> u32 {
    if f.len() >= CONTACT_FRAME_LEN {
        u32le(f, CONTACT_FRAME_LEN - 4)
    } else {
        0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SelfInfo {
    pub pubkey: [u8; 32],
    pub name: String,
    pub freq_khz: u32,
    pub bw_hz: u32,
    pub sf: u8,
    pub cr: u8,
    pub tx_power: u8,
}

pub fn parse_self_info(f: &[u8]) -> Option<SelfInfo> {
    if f.len() < 58 || f[0] != resp::SELF_INFO {
        return None;
    }
    let mut pubkey = [0u8; 32];
    pubkey.copy_from_slice(&f[4..36]);
    Some(SelfInfo {
        pubkey,
        tx_power: f[2],
        freq_khz: u32le(f, 48),
        bw_hz: u32le(f, 52),
        sf: f[56],
        cr: f[57],
        name: String::from_utf8_lossy(&f[58..]).into_owned(),
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceInfo {
    pub fw_ver: u8,
    pub max_channels: u8,
    pub build_date: String,
    pub model: String,
    pub version: String,
}

pub fn parse_device_info(f: &[u8]) -> Option<DeviceInfo> {
    if f.len() < 4 || f[0] != resp::DEVICE_INFO {
        return None;
    }
    let field = |from: usize, len: usize| {
        if f.len() >= from + len {
            cstr(&f[from..from + len])
        } else {
            String::new()
        }
    };
    Some(DeviceInfo {
        fw_ver: f[1],
        max_channels: f[3],
        build_date: field(8, 12),
        model: field(20, 40),
        version: field(60, 20),
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChannelInfo {
    pub idx: u8,
    pub name: String,
}

pub fn parse_channel_info(f: &[u8]) -> Option<ChannelInfo> {
    if f.len() < 34 || f[0] != resp::CHANNEL_INFO {
        return None;
    }
    Some(ChannelInfo {
        idx: f[1],
        name: cstr(&f[2..34]),
    })
}

/// A message taken from the radio's offline queue.
#[derive(Debug, Clone, PartialEq)]
pub struct InboundMessage {
    pub kind: MsgKind,
    /// Sender pubkey prefix (DMs only).
    pub prefix: Option<[u8; 6]>,
    pub channel: Option<u8>,
    /// SNR in dB, when the frame carried it.
    pub snr: Option<f32>,
    pub path_len: u8,
    pub txt_type: u8,
    pub sender_ts: u32,
    pub text: String,
    /// The frame normalised to the v3 layout (what an app with
    /// `app_target_ver >= 3` would receive).
    pub frame_v3: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgKind {
    Direct,
    Channel,
    ChannelData,
    Other,
}

impl MsgKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MsgKind::Direct => "dm",
            MsgKind::Channel => "channel",
            MsgKind::ChannelData => "channel_data",
            MsgKind::Other => "other",
        }
    }
}

/// Is this code something `CMD_SYNC_NEXT_MESSAGE` hands out (as opposed to
/// `NO_MORE_MESSAGES` or an error)?
pub fn is_queued_message(code: u8) -> bool {
    !matches!(code, resp::NO_MORE_MESSAGES | resp::ERR | resp::OK) && !is_push(code)
}

/// Parse a frame returned by `CMD_SYNC_NEXT_MESSAGE`. Unknown future message
/// codes come back as [`MsgKind::Other`] with the frame kept verbatim.
pub fn parse_inbound(f: &[u8]) -> Option<InboundMessage> {
    let code = *f.first()?;
    let (v3, snr_known) = match code {
        resp::CONTACT_MSG_RECV => {
            let mut v = vec![resp::CONTACT_MSG_RECV_V3, 0, 0, 0];
            v.extend_from_slice(&f[1..]);
            (v, false)
        }
        resp::CHANNEL_MSG_RECV => {
            let mut v = vec![resp::CHANNEL_MSG_RECV_V3, 0, 0, 0];
            v.extend_from_slice(&f[1..]);
            (v, false)
        }
        _ => (f.to_vec(), true),
    };
    let snr = |v: &[u8]| {
        if snr_known {
            Some(v[1] as i8 as f32 / 4.0)
        } else {
            None
        }
    };
    match v3[0] {
        resp::CONTACT_MSG_RECV_V3 => {
            if v3.len() < 16 {
                return None;
            }
            let mut prefix = [0u8; 6];
            prefix.copy_from_slice(&v3[4..10]);
            let txt_type = v3[11];
            let mut at = 16;
            if txt_type == txt::SIGNED_PLAIN {
                at += 4;
            }
            let text = String::from_utf8_lossy(v3.get(at..).unwrap_or(&[])).into_owned();
            Some(InboundMessage {
                kind: MsgKind::Direct,
                prefix: Some(prefix),
                channel: None,
                snr: snr(&v3),
                path_len: v3[10],
                txt_type,
                sender_ts: u32le(&v3, 12),
                text,
                frame_v3: v3,
            })
        }
        resp::CHANNEL_MSG_RECV_V3 => {
            if v3.len() < 11 {
                return None;
            }
            Some(InboundMessage {
                kind: MsgKind::Channel,
                prefix: None,
                channel: Some(v3[4]),
                snr: snr(&v3),
                path_len: v3[5],
                txt_type: v3[6],
                sender_ts: u32le(&v3, 7),
                text: String::from_utf8_lossy(&v3[11..]).into_owned(),
                frame_v3: v3,
            })
        }
        resp::CHANNEL_DATA_RECV => {
            if v3.len() < 9 {
                return None;
            }
            Some(InboundMessage {
                kind: MsgKind::ChannelData,
                prefix: None,
                channel: Some(v3[4]),
                snr: snr(&v3),
                path_len: v3[5],
                txt_type: 0,
                sender_ts: 0,
                text: String::new(),
                frame_v3: v3,
            })
        }
        _ => Some(InboundMessage {
            kind: MsgKind::Other,
            prefix: None,
            channel: None,
            snr: None,
            path_len: 0,
            txt_type: 0,
            sender_ts: 0,
            text: String::new(),
            frame_v3: v3,
        }),
    }
}

/// Re-encode a stored v3 message frame for an app that announced
/// `app_target_ver` (the firmware only switches layout at version 3).
pub fn frame_for_app(frame_v3: &[u8], app_target_ver: u8) -> Vec<u8> {
    if app_target_ver >= 3 || frame_v3.len() < 4 {
        return frame_v3.to_vec();
    }
    let legacy = match frame_v3[0] {
        resp::CONTACT_MSG_RECV_V3 => resp::CONTACT_MSG_RECV,
        resp::CHANNEL_MSG_RECV_V3 => resp::CHANNEL_MSG_RECV,
        _ => return frame_v3.to_vec(),
    };
    let mut v = vec![legacy];
    v.extend_from_slice(&frame_v3[4..]);
    v
}

/// Build `CMD_SEND_TXT_MSG`.
pub fn build_send_txt(prefix: &[u8; 6], ts: u32, attempt: u8, text: &str) -> Vec<u8> {
    let mut v = vec![cmd::SEND_TXT_MSG, txt::PLAIN, attempt];
    v.extend_from_slice(&ts.to_le_bytes());
    v.extend_from_slice(prefix);
    v.extend_from_slice(text.as_bytes());
    v
}

/// Build `CMD_SEND_CHANNEL_TXT_MSG`.
pub fn build_send_channel(idx: u8, ts: u32, text: &str) -> Vec<u8> {
    let mut v = vec![cmd::SEND_CHANNEL_TXT_MSG, txt::PLAIN, idx];
    v.extend_from_slice(&ts.to_le_bytes());
    v.extend_from_slice(text.as_bytes());
    v
}

/// Fields of a `RESP_CODE_SENT` frame: (flood, expected_ack, est_timeout_ms).
pub fn parse_sent(f: &[u8]) -> Option<(bool, u32, u32)> {
    if f.len() < 10 || f[0] != resp::SENT {
        return None;
    }
    Some((f[1] == 1, u32le(f, 2), u32le(f, 6)))
}

/// ack CRC of a `PUSH_CODE_SEND_CONFIRMED` frame.
pub fn parse_send_confirmed(f: &[u8]) -> Option<(u32, u32)> {
    if f.len() < 9 || f[0] != push::SEND_CONFIRMED {
        return None;
    }
    Some((u32le(f, 1), u32le(f, 5)))
}

/// Parse an outgoing `CMD_SEND_TXT_MSG` from an app:
/// (txt_type, attempt, timestamp, prefix, text).
pub fn parse_send_txt_cmd(f: &[u8]) -> Option<(u8, u8, u32, [u8; 6], String)> {
    if f.len() < 14 || f[0] != cmd::SEND_TXT_MSG {
        return None;
    }
    let mut p = [0u8; 6];
    p.copy_from_slice(&f[7..13]);
    Some((
        f[1],
        f[2],
        u32le(f, 3),
        p,
        String::from_utf8_lossy(&f[13..]).into_owned(),
    ))
}

/// Parse an outgoing `CMD_SEND_CHANNEL_TXT_MSG`: (txt_type, idx, timestamp, text).
pub fn parse_send_channel_cmd(f: &[u8]) -> Option<(u8, u8, u32, String)> {
    if f.len() < 7 || f[0] != cmd::SEND_CHANNEL_TXT_MSG {
        return None;
    }
    Some((
        f[1],
        f[2],
        u32le(f, 3),
        String::from_utf8_lossy(&f[7..]).into_owned(),
    ))
}

/// Contact frames for an offline `CMD_GET_CONTACTS` answer, mirroring the
/// firmware's `since` filter and `END_OF_CONTACTS` payload.
pub fn contacts_listing(contacts: &[Vec<u8>], since: u32) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut start = vec![resp::CONTACTS_START];
    start.extend_from_slice(&(contacts.len() as u32).to_le_bytes());
    out.push(start);
    let mut most_recent = 0u32;
    for c in contacts {
        let lm = contact_lastmod(c);
        if lm > since {
            let mut f = c.clone();
            f[0] = resp::CONTACT;
            out.push(f);
            most_recent = most_recent.max(lm);
        }
    }
    let mut end = vec![resp::END_OF_CONTACTS];
    end.extend_from_slice(&most_recent.to_le_bytes());
    out.push(end);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_handles_split_and_noise() {
        let mut d = FrameDecoder::new(b'>', MAX_FRAME_SIZE);
        let f1 = encode_frame(b'>', &[5, 1, 2]);
        let f2 = encode_frame(b'>', &[0x83]);
        d.push(b"garbage");
        d.push(&f1[..2]);
        assert_eq!(d.next_frame(), None);
        d.push(&f1[2..]);
        d.push(&f2);
        assert_eq!(d.next_frame(), Some(vec![5, 1, 2]));
        assert_eq!(d.next_frame(), Some(vec![0x83]));
        assert_eq!(d.next_frame(), None);
    }

    #[test]
    fn decoder_skips_oversize_and_empty() {
        let mut d = FrameDecoder::new(b'<', MAX_FRAME_SIZE);
        d.push(&encode_frame(b'<', &[]));
        d.push(&encode_frame(b'<', &[7u8; 200]));
        d.push(&encode_frame(b'<', &[1]));
        assert_eq!(d.next_frame(), Some(vec![1]));
    }

    #[test]
    fn legacy_frames_round_trip() {
        let mut legacy = vec![resp::CONTACT_MSG_RECV, 1, 2, 3, 4, 5, 6, 0xff, 0];
        legacy.extend_from_slice(&1234u32.to_le_bytes());
        legacy.extend_from_slice(b"hi");
        let m = parse_inbound(&legacy).unwrap();
        assert_eq!(m.kind, MsgKind::Direct);
        assert_eq!(m.text, "hi");
        assert_eq!(m.sender_ts, 1234);
        assert_eq!(m.snr, None);
        assert_eq!(frame_for_app(&m.frame_v3, 1), legacy);
        assert_eq!(frame_for_app(&m.frame_v3, 3)[0], resp::CONTACT_MSG_RECV_V3);
    }

    #[test]
    fn signed_dm_skips_prefix() {
        let mut f = vec![
            resp::CONTACT_MSG_RECV_V3,
            8,
            0,
            0,
            1,
            2,
            3,
            4,
            5,
            6,
            2,
            txt::SIGNED_PLAIN,
        ];
        f.extend_from_slice(&9u32.to_le_bytes());
        f.extend_from_slice(&[0xaa; 4]);
        f.extend_from_slice(b"yo");
        let m = parse_inbound(&f).unwrap();
        assert_eq!(m.text, "yo");
        assert_eq!(m.snr, Some(2.0));
    }

    #[test]
    fn expectations() {
        assert_eq!(
            expect_for(&[cmd::REBOOT, b'r', b'e', b'b', b'o', b'o', b't']),
            Expect::Nothing
        );
        assert_eq!(expect_for(&[cmd::GET_CONTACTS]), Expect::Contacts);
        assert_eq!(
            expect_for(&[cmd::SEND_TELEMETRY_REQ, 0, 0, 0]),
            Expect::SelfTelemetry
        );
        assert_eq!(expect_for(&[200]), Expect::AnyOne);
    }
}
