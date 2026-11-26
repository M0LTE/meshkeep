// SPDX-License-Identifier: AGPL-3.0-or-later
//! Just enough of the firmware's group channel crypto to recognise repeats of
//! our own channel messages in the radio's RX log (`Utils::encryptThenMAC`,
//! `Utils::MACThenDecrypt`, `BaseChatMesh::setChannel`).

use aes::Aes128;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit, generic_array::GenericArray};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

/// `PAYLOAD_TYPE_GRP_TXT`.
pub const PAYLOAD_TYPE_GRP_TXT: u8 = 0x05;
const MAC_SIZE: usize = 2;
const BLOCK: usize = 16;

/// A channel's 128-bit secret, as `RESP_CODE_CHANNEL_INFO` carries it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelKey {
    pub idx: u8,
    pub secret: [u8; 16],
    /// First byte of SHA-256 of the secret: the hash a packet is addressed to.
    pub hash: u8,
}

impl ChannelKey {
    pub fn new(idx: u8, secret: [u8; 16]) -> ChannelKey {
        let hash = Sha256::digest(secret)[0];
        ChannelKey { idx, secret, hash }
    }

    /// The HMAC key is the full 32 byte secret field, zero padded.
    fn mac_key(&self) -> [u8; 32] {
        let mut k = [0u8; 32];
        k[..16].copy_from_slice(&self.secret);
        k
    }

    fn mac(&self, data: &[u8]) -> [u8; MAC_SIZE] {
        let mut m = <Hmac<Sha256> as Mac>::new_from_slice(&self.mac_key()).expect("any key size");
        m.update(data);
        let out = m.finalize().into_bytes();
        [out[0], out[1]]
    }

    /// Decrypt a group payload (after the channel hash byte): MAC, then
    /// AES-128-ECB blocks. `None` if the MAC does not match.
    pub fn open(&self, mac_and_data: &[u8]) -> Option<Vec<u8>> {
        if mac_and_data.len() <= MAC_SIZE {
            return None;
        }
        let (mac, data) = mac_and_data.split_at(MAC_SIZE);
        if !data.len().is_multiple_of(BLOCK) || self.mac(data) != mac {
            return None;
        }
        let aes = Aes128::new(GenericArray::from_slice(&self.secret));
        let mut out = data.to_vec();
        for b in out.chunks_mut(BLOCK) {
            aes.decrypt_block(GenericArray::from_mut_slice(b));
        }
        Some(out)
    }

    /// Encrypt like the firmware (zero padded to a block, MAC first).
    pub fn seal(&self, plain: &[u8]) -> Vec<u8> {
        let aes = Aes128::new(GenericArray::from_slice(&self.secret));
        let mut data = plain.to_vec();
        data.resize(plain.len().div_ceil(BLOCK).max(1) * BLOCK, 0);
        for b in data.chunks_mut(BLOCK) {
            aes.encrypt_block(GenericArray::from_mut_slice(b));
        }
        let mut out = self.mac(&data).to_vec();
        out.extend_from_slice(&data);
        out
    }
}

/// A group text message read out of a raw packet.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupText {
    pub idx: u8,
    pub timestamp: u32,
    /// `<sender name>: <text>` as sent.
    pub text: Vec<u8>,
}

/// Payload of a raw over-the-air packet (`Packet::readFrom`): header,
/// optional transport codes, path length, path, payload.
pub fn packet_payload(raw: &[u8]) -> Option<(u8, &[u8])> {
    let header = *raw.first()?;
    let route = header & 0x03;
    let payload_type = (header >> 2) & 0x0f;
    let mut i = 1;
    if route == 0x00 || route == 0x03 {
        i += 4;
    }
    let path_len = *raw.get(i)?;
    i += 1;
    let hash_size = (path_len >> 6) as usize + 1;
    if hash_size == 4 {
        return None; // reserved encoding
    }
    i += (path_len & 63) as usize * hash_size;
    if i >= raw.len() {
        return None;
    }
    Some((payload_type, &raw[i..]))
}

/// Decrypt a raw packet if it is group text on one of `keys`.
pub fn read_group_text(raw: &[u8], keys: &[ChannelKey]) -> Option<GroupText> {
    let (ptype, payload) = packet_payload(raw)?;
    if ptype != PAYLOAD_TYPE_GRP_TXT || payload.len() < 1 + MAC_SIZE + BLOCK {
        return None;
    }
    for k in keys.iter().filter(|k| k.hash == payload[0]) {
        if let Some(plain) = k.open(&payload[1..]) {
            if plain.len() < 5 || plain[4] >> 2 != 0 {
                return None;
            }
            let end = plain[5..]
                .iter()
                .position(|&b| b == 0)
                .map_or(plain.len(), |p| p + 5);
            return Some(GroupText {
                idx: k.idx,
                timestamp: u32::from_le_bytes([plain[0], plain[1], plain[2], plain[3]]),
                text: plain[5..end].to_vec(),
            });
        }
    }
    None
}

/// Build the raw packet the firmware floods for a channel message
/// (`BaseChatMesh::sendGroupMessage`), as heard after `hops` repeaters.
/// Used by the simulator.
pub fn group_text_packet(key: &ChannelKey, timestamp: u32, text: &[u8], hops: &[u8]) -> Vec<u8> {
    let mut plain = timestamp.to_le_bytes().to_vec();
    plain.push(0); // TXT_TYPE_PLAIN
    plain.extend_from_slice(text);
    plain.push(0);
    let mut raw = vec![(PAYLOAD_TYPE_GRP_TXT << 2) | 0x01, hops.len() as u8];
    raw.extend_from_slice(hops);
    raw.push(key.hash);
    raw.extend_from_slice(&key.seal(&plain));
    raw
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_channel_hash_matches_firmware() {
        // PUBLIC_GROUP_PSK "izOH6cXN6mrJ5e26oRXNcg==".
        let secret = [
            0x8b, 0x33, 0x87, 0xe9, 0xc5, 0xcd, 0xea, 0x6a, 0xc9, 0xe5, 0xed, 0xba, 0xa1, 0x15,
            0xcd, 0x72,
        ];
        let k = ChannelKey::new(0, secret);
        assert_eq!(k.hash, 0x11);
    }

    #[test]
    fn round_trip_and_wrong_key() {
        let k = ChannelKey::new(1, [7; 16]);
        let other = ChannelKey::new(2, [8; 16]);
        let raw = group_text_packet(&k, 1234, b"Node: hello", &[0xaa, 0xbb]);
        let g = read_group_text(&raw, &[other, k]).unwrap();
        assert_eq!(
            g,
            GroupText {
                idx: 1,
                timestamp: 1234,
                text: b"Node: hello".to_vec()
            }
        );
        assert!(read_group_text(&raw, &[other]).is_none());
        let mut bad = raw.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert!(read_group_text(&bad, &[k]).is_none());
    }
}
