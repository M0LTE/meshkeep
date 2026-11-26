// SPDX-License-Identifier: AGPL-3.0-or-later
//! SQLite storage: messages, per-client sync cursors, cached radio state and
//! web sessions.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::protocol::{self, InboundMessage, MsgKind};

/// A brand-new per-address device gets at most this many earlier messages.
pub const NEW_DEVICE_BACKLOG: i64 = 100;

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Message {
    pub id: i64,
    /// `dm:<12 hex>` or `ch:<index>`.
    pub conv: String,
    pub direction: String,
    pub kind: String,
    pub received_at: u64,
    pub sender_ts: u32,
    pub sender: Option<String>,
    pub text: String,
    pub snr: Option<f32>,
    pub path_len: Option<u8>,
    pub txt_type: u8,
    pub status: Option<String>,
    pub channel_name: Option<String>,
    /// Times the radio heard a repeater pass on this sent channel message.
    pub heard: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContactRow {
    pub pubkey: String,
    pub prefix: String,
    pub name: String,
    pub kind: u8,
    pub last_advert: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelRow {
    pub idx: u8,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Conversation {
    pub conv: String,
    pub last_id: i64,
    pub last_at: u64,
    pub last_text: String,
    pub last_direction: String,
}

pub fn conv_dm(prefix: &[u8]) -> String {
    format!("dm:{}", protocol::hex(&prefix[..6]))
}

pub fn conv_channel(idx: u8) -> String {
    format!("ch:{idx}")
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    conv TEXT NOT NULL,
    direction TEXT NOT NULL,
    kind TEXT NOT NULL,
    received_at INTEGER NOT NULL,
    sender_ts INTEGER NOT NULL,
    sender TEXT,
    text TEXT NOT NULL,
    snr REAL,
    path_len INTEGER,
    txt_type INTEGER NOT NULL DEFAULT 0,
    frame BLOB,
    status TEXT,
    ack_crc INTEGER,
    channel_name TEXT
);
CREATE INDEX IF NOT EXISTS messages_conv ON messages(conv, id);
CREATE INDEX IF NOT EXISTS messages_ack ON messages(ack_crc);
CREATE TABLE IF NOT EXISTS cursors (
    client TEXT PRIMARY KEY,
    last_id INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS contacts (
    pubkey BLOB PRIMARY KEY,
    frame BLOB NOT NULL,
    name TEXT NOT NULL,
    kind INTEGER NOT NULL,
    last_advert INTEGER NOT NULL,
    lastmod INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS channels (
    idx INTEGER PRIMARY KEY,
    frame BLOB NOT NULL,
    name TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS kv (
    key TEXT PRIMARY KEY,
    value BLOB NOT NULL
);
CREATE TABLE IF NOT EXISTS sessions (
    token TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL
);
"#;

fn row_to_message(r: &rusqlite::Row) -> rusqlite::Result<Message> {
    Ok(Message {
        id: r.get(0)?,
        conv: r.get(1)?,
        direction: r.get(2)?,
        kind: r.get(3)?,
        received_at: r.get::<_, i64>(4)? as u64,
        sender_ts: r.get::<_, i64>(5)? as u32,
        sender: r.get(6)?,
        text: r.get(7)?,
        snr: r.get::<_, Option<f64>>(8)?.map(|v| v as f32),
        path_len: r.get::<_, Option<i64>>(9)?.map(|v| v as u8),
        txt_type: r.get::<_, i64>(10)? as u8,
        status: r.get(11)?,
        channel_name: r.get(12)?,
        heard: r.get::<_, Option<i64>>(13)?.unwrap_or(0) as u32,
    })
}

const MSG_COLS: &str = "id, conv, direction, kind, received_at, sender_ts, sender, text, snr, path_len, txt_type, status, channel_name, heard";

impl Store {
    pub fn open(path: &Path) -> rusqlite::Result<Store> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> rusqlite::Result<Store> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> rusqlite::Result<Store> {
        conn.execute_batch(SCHEMA)?;
        // 0.2.0: mirrors of sent messages remember which client sent them.
        let has_origin: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('messages') WHERE name = 'origin_client'")?
            .exists([])?;
        if !has_origin {
            conn.execute_batch("ALTER TABLE messages ADD COLUMN origin_client TEXT")?;
        }
        let has_heard: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('messages') WHERE name = 'heard'")?
            .exists([])?;
        if !has_heard {
            conn.execute_batch("ALTER TABLE messages ADD COLUMN heard INTEGER NOT NULL DEFAULT 0")?;
        }
        Ok(Store {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn db(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Run raw SQL. For tests that need the database to misbehave.
    #[doc(hidden)]
    pub fn execute_batch_for_tests(&self, sql: &str) -> rusqlite::Result<()> {
        self.db().execute_batch(sql)
    }

    pub fn max_message_id(&self) -> i64 {
        self.db()
            .query_row("SELECT COALESCE(MAX(id), 0) FROM messages", [], |r| {
                r.get(0)
            })
            .unwrap_or(0)
    }

    /// Store a message drained from the radio. Returns the new id.
    pub fn insert_inbound(
        &self,
        m: &InboundMessage,
        received_at: u64,
        channel_name: Option<&str>,
    ) -> rusqlite::Result<i64> {
        let (conv, sender, text) = match m.kind {
            MsgKind::Direct => {
                // Room server posts are signed: the author's key prefix
                // comes before the text.
                let author = (m.txt_type == protocol::txt::SIGNED_PLAIN && m.frame_v3.len() >= 20)
                    .then(|| protocol::hex(&m.frame_v3[16..20]));
                (
                    conv_dm(&m.prefix.unwrap_or_default()),
                    author,
                    m.text.clone(),
                )
            }
            MsgKind::Channel => {
                let idx = m.channel.unwrap_or(0);
                // Channel text arrives as "<sender name>: <text>".
                match m.text.split_once(": ") {
                    Some((s, t)) => (conv_channel(idx), Some(s.to_string()), t.to_string()),
                    None => (conv_channel(idx), None, m.text.clone()),
                }
            }
            MsgKind::ChannelData => (conv_channel(m.channel.unwrap_or(0)), None, String::new()),
            MsgKind::Other => ("other".to_string(), None, String::new()),
        };
        let db = self.db();
        db.execute(
            "INSERT INTO messages (conv, direction, kind, received_at, sender_ts, sender, text, snr, path_len, txt_type, frame, channel_name)
             VALUES (?1, 'in', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                conv,
                m.kind.as_str(),
                received_at as i64,
                m.sender_ts as i64,
                sender,
                text,
                m.snr.map(|v| v as f64),
                m.path_len as i64,
                m.txt_type as i64,
                m.frame_v3,
                channel_name,
            ],
        )?;
        Ok(db.last_insert_rowid())
    }

    /// Record an outgoing message. Returns the id.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_outbound(
        &self,
        conv: &str,
        kind: MsgKind,
        sent_at: u64,
        sender_ts: u32,
        sender: Option<&str>,
        text: &str,
        status: &str,
        channel_name: Option<&str>,
    ) -> rusqlite::Result<i64> {
        let db = self.db();
        db.execute(
            "INSERT INTO messages (conv, direction, kind, received_at, sender_ts, sender, text, status, channel_name)
             VALUES (?1, 'out', ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![conv, kind.as_str(), sent_at as i64, sender_ts as i64, sender, text, status, channel_name],
        )?;
        Ok(db.last_insert_rowid())
    }

    /// An outgoing DM already recorded with the same timestamp and text (an
    /// app retry), if any.
    pub fn find_outbound_dm(&self, conv: &str, sender_ts: u32, text: &str) -> Option<i64> {
        self.db()
            .query_row(
                "SELECT id FROM messages WHERE conv = ?1 AND direction = 'out' AND sender_ts = ?2 AND text = ?3 ORDER BY id DESC LIMIT 1",
                params![conv, sender_ts as i64, text],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }

    pub fn set_status(&self, id: i64, status: &str, ack_crc: Option<u32>) {
        let db = self.db();
        let _ = match ack_crc {
            Some(a) => db.execute(
                "UPDATE messages SET status = ?2, ack_crc = ?3 WHERE id = ?1",
                params![id, status, a as i64],
            ),
            None => db.execute(
                "UPDATE messages SET status = ?2 WHERE id = ?1",
                params![id, status],
            ),
        };
    }

    /// Count one more heard repeat of a sent message. Returns the new count.
    pub fn add_heard(&self, id: i64) -> u32 {
        let db = self.db();
        let _ = db.execute(
            "UPDATE messages SET heard = heard + 1 WHERE id = ?1",
            params![id],
        );
        db.query_row(
            "SELECT heard FROM messages WHERE id = ?1",
            params![id],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0) as u32
    }

    /// Mark the outgoing message waiting for this ack as delivered.
    pub fn confirm_ack(&self, ack: u32) -> Option<i64> {
        let db = self.db();
        let id: Option<i64> = db
            .query_row(
                "SELECT id FROM messages WHERE direction = 'out' AND ack_crc = ?1 ORDER BY id DESC LIMIT 1",
                params![ack as i64],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten();
        if let Some(id) = id {
            let _ = db.execute(
                "UPDATE messages SET status = 'delivered' WHERE id = ?1",
                params![id],
            );
        }
        id
    }

    pub fn message(&self, id: i64) -> Option<Message> {
        self.db()
            .query_row(
                &format!("SELECT {MSG_COLS} FROM messages WHERE id = ?1"),
                params![id],
                row_to_message,
            )
            .optional()
            .ok()
            .flatten()
    }

    /// Messages of a conversation, newest last, older than `before` if given.
    pub fn messages(&self, conv: &str, before: Option<i64>, limit: u32) -> Vec<Message> {
        let db = self.db();
        let mut st = db
            .prepare(&format!(
                "SELECT {MSG_COLS} FROM messages WHERE conv = ?1 AND id < ?2 AND direction != 'mirror' ORDER BY id DESC LIMIT ?3"
            ))
            .expect("prepare");
        let mut v: Vec<Message> = st
            .query_map(
                params![conv, before.unwrap_or(i64::MAX), limit as i64],
                row_to_message,
            )
            .map(|it| it.filter_map(Result::ok).collect())
            .unwrap_or_default();
        v.reverse();
        v
    }

    pub fn conversations(&self) -> Vec<Conversation> {
        let db = self.db();
        let mut st = db
            .prepare(
                "SELECT m.conv, m.id, m.received_at, m.text, m.direction FROM messages m
                 JOIN (SELECT conv, MAX(id) AS mid FROM messages WHERE kind IN ('dm','channel') AND direction != 'mirror' GROUP BY conv) l
                 ON m.id = l.mid ORDER BY m.id DESC",
            )
            .expect("prepare");
        st.query_map([], |r| {
            Ok(Conversation {
                conv: r.get(0)?,
                last_id: r.get(1)?,
                last_at: r.get::<_, i64>(2)? as u64,
                last_text: r.get(3)?,
                last_direction: r.get(4)?,
            })
        })
        .map(|it| it.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }

    // ---- sync cursors

    pub fn cursor(&self, client: &str) -> Option<i64> {
        self.db()
            .query_row(
                "SELECT last_id FROM cursors WHERE client = ?1",
                params![client],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }

    /// Create the cursor at `start` if the client is new. Returns the cursor.
    pub fn ensure_cursor(&self, client: &str, start: i64) -> i64 {
        let db = self.db();
        let _ = db.execute(
            "INSERT OR IGNORE INTO cursors (client, last_id) VALUES (?1, ?2)",
            params![client, start],
        );
        db.query_row(
            "SELECT last_id FROM cursors WHERE client = ?1",
            params![client],
            |r| r.get(0),
        )
        .unwrap_or(start)
    }

    /// Advance (never rewind) a cursor.
    pub fn advance_cursor(&self, client: &str, to: i64) {
        let _ = self.db().execute(
            "UPDATE cursors SET last_id = MAX(last_id, ?2) WHERE client = ?1",
            params![client, to],
        );
    }

    /// Next message an app should receive after `after`: (id, v3 frame).
    /// Next frame an app should receive after `after`: (id, v3 frame).
    /// Received messages, plus mirrors of messages sent by other clients.
    pub fn next_for_app(&self, after: i64, client: &str) -> Option<(i64, Vec<u8>)> {
        self.db()
            .query_row(
                "SELECT id, frame FROM messages WHERE id > ?1 AND frame IS NOT NULL
                 AND (direction = 'in' OR (direction = 'mirror' AND (origin_client IS NULL OR origin_client != ?2)))
                 ORDER BY id LIMIT 1",
                params![after, client],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .ok()
            .flatten()
    }

    /// Store a mirror of a sent message for the other apps: never shown in
    /// the web view, never handed to `origin_client`.
    pub fn insert_mirror(
        &self,
        conv: &str,
        kind: MsgKind,
        at: u64,
        frame: &[u8],
        origin_client: Option<&str>,
    ) -> rusqlite::Result<i64> {
        let db = self.db();
        db.execute(
            "INSERT INTO messages (conv, direction, kind, received_at, sender_ts, text, frame, origin_client)
             VALUES (?1, 'mirror', ?2, ?3, 0, '', ?4, ?5)",
            params![conv, kind.as_str(), at as i64, frame, origin_client],
        )?;
        Ok(db.last_insert_rowid())
    }

    /// Id of the n-th newest inbound message (for new-device backlog limits).
    pub fn nth_newest_inbound_id(&self, n: i64) -> i64 {
        self.db()
            .query_row(
                "SELECT COALESCE(MIN(id), 0) FROM (SELECT id FROM messages WHERE direction = 'in' AND frame IS NOT NULL ORDER BY id DESC LIMIT ?1)",
                params![n],
                |r| r.get(0),
            )
            .unwrap_or(0)
    }

    // ---- cached radio state

    pub fn upsert_contact(&self, frame: &[u8]) {
        let Some(c) = protocol::parse_contact(frame) else {
            return;
        };
        let mut f = frame[..protocol::CONTACT_FRAME_LEN].to_vec();
        f[0] = protocol::resp::CONTACT;
        let _ = self.db().execute(
            "INSERT INTO contacts (pubkey, frame, name, kind, last_advert, lastmod) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(pubkey) DO UPDATE SET frame = excluded.frame, name = excluded.name, kind = excluded.kind,
             last_advert = excluded.last_advert, lastmod = excluded.lastmod",
            params![
                c.pubkey.to_vec(),
                f,
                c.name,
                c.kind as i64,
                c.last_advert as i64,
                c.lastmod as i64
            ],
        );
    }

    pub fn delete_contact(&self, pubkey: &[u8]) {
        let _ = self.db().execute(
            "DELETE FROM contacts WHERE pubkey = ?1",
            params![pubkey.to_vec()],
        );
    }

    /// Replace the whole contact cache.
    pub fn replace_contacts(&self, frames: &[Vec<u8>]) {
        {
            let _ = self.db().execute("DELETE FROM contacts", []);
        }
        for f in frames {
            self.upsert_contact(f);
        }
    }

    pub fn contact_frames(&self) -> Vec<Vec<u8>> {
        let db = self.db();
        let mut st = db
            .prepare("SELECT frame FROM contacts ORDER BY rowid")
            .expect("prepare");
        st.query_map([], |r| r.get(0))
            .map(|it| it.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    pub fn contact_frame(&self, pubkey: &[u8]) -> Option<Vec<u8>> {
        self.db()
            .query_row(
                "SELECT frame FROM contacts WHERE pubkey = ?1",
                params![pubkey.to_vec()],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }

    pub fn contacts(&self) -> Vec<ContactRow> {
        let db = self.db();
        let mut st = db
            .prepare(
                "SELECT pubkey, name, kind, last_advert FROM contacts ORDER BY name COLLATE NOCASE",
            )
            .expect("prepare");
        st.query_map([], |r| {
            let pk: Vec<u8> = r.get(0)?;
            Ok(ContactRow {
                pubkey: protocol::hex(&pk),
                prefix: protocol::hex(&pk[..6.min(pk.len())]),
                name: r.get(1)?,
                kind: r.get::<_, i64>(2)? as u8,
                last_advert: r.get::<_, i64>(3)? as u32,
            })
        })
        .map(|it| it.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }

    /// Full public key for a 6 byte prefix, if known.
    pub fn contact_by_prefix(&self, prefix: &[u8]) -> Option<(Vec<u8>, String)> {
        let db = self.db();
        let mut st = db.prepare("SELECT pubkey, name FROM contacts").ok()?;
        let rows = st
            .query_map([], |r| {
                Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, String>(1)?))
            })
            .ok()?;
        rows.filter_map(Result::ok)
            .find(|(pk, _)| pk.len() >= 6 && pk[..6] == prefix[..6])
    }

    pub fn upsert_channel(&self, frame: &[u8]) {
        let Some(c) = protocol::parse_channel_info(frame) else {
            return;
        };
        let _ = self.db().execute(
            "INSERT INTO channels (idx, frame, name) VALUES (?1, ?2, ?3)
             ON CONFLICT(idx) DO UPDATE SET frame = excluded.frame, name = excluded.name",
            params![c.idx as i64, frame.to_vec(), c.name],
        );
    }

    pub fn channel_frame(&self, idx: u8) -> Option<Vec<u8>> {
        self.db()
            .query_row(
                "SELECT frame FROM channels WHERE idx = ?1",
                params![idx as i64],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }

    /// Every cached `RESP_CODE_CHANNEL_INFO` frame.
    pub fn channel_frames(&self) -> Vec<Vec<u8>> {
        let db = self.db();
        let mut st = db
            .prepare("SELECT frame FROM channels ORDER BY idx")
            .expect("prepare");
        st.query_map([], |r| r.get(0))
            .map(|it| it.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    pub fn channel_name(&self, idx: u8) -> Option<String> {
        self.channel_frame(idx)
            .and_then(|f| protocol::parse_channel_info(&f))
            .map(|c| c.name)
            .filter(|n| !n.is_empty())
    }

    pub fn channels(&self) -> Vec<ChannelRow> {
        let db = self.db();
        let mut st = db
            .prepare("SELECT idx, name FROM channels WHERE name != '' ORDER BY idx")
            .expect("prepare");
        st.query_map([], |r| {
            Ok(ChannelRow {
                idx: r.get::<_, i64>(0)? as u8,
                name: r.get(1)?,
            })
        })
        .map(|it| it.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }

    pub fn put_kv(&self, key: &str, value: &[u8]) {
        let _ = self.db().execute(
            "INSERT INTO kv (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value.to_vec()],
        );
    }

    pub fn get_kv(&self, key: &str) -> Option<Vec<u8>> {
        self.db()
            .query_row("SELECT value FROM kv WHERE key = ?1", params![key], |r| {
                r.get(0)
            })
            .optional()
            .ok()
            .flatten()
    }

    // ---- web sessions

    pub fn add_session(&self, token: &str, now: u64) {
        let _ = self.db().execute(
            "INSERT INTO sessions (token, created_at) VALUES (?1, ?2)",
            params![token, now as i64],
        );
    }

    pub fn session_valid(&self, token: &str, now: u64, max_age: u64) -> bool {
        let created: Option<i64> = self
            .db()
            .query_row(
                "SELECT created_at FROM sessions WHERE token = ?1",
                params![token],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten();
        matches!(created, Some(c) if now.saturating_sub(c as u64) < max_age)
    }

    pub fn delete_session(&self, token: &str) {
        let _ = self
            .db()
            .execute("DELETE FROM sessions WHERE token = ?1", params![token]);
    }

    pub fn prune_sessions(&self, now: u64, max_age: u64) {
        let _ = self.db().execute(
            "DELETE FROM sessions WHERE created_at < ?1",
            params![now.saturating_sub(max_age) as i64],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::resp;

    fn dm(text: &str) -> InboundMessage {
        let mut f = vec![
            resp::CONTACT_MSG_RECV_V3,
            4,
            0,
            0,
            1,
            2,
            3,
            4,
            5,
            6,
            0xff,
            0,
        ];
        f.extend_from_slice(&5u32.to_le_bytes());
        f.extend_from_slice(text.as_bytes());
        protocol::parse_inbound(&f).unwrap()
    }

    #[test]
    fn cursors_never_rewind() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.ensure_cursor("a", 3), 3);
        assert_eq!(s.ensure_cursor("a", 0), 3);
        s.advance_cursor("a", 2);
        assert_eq!(s.cursor("a"), Some(3));
        s.advance_cursor("a", 9);
        assert_eq!(s.cursor("a"), Some(9));
    }

    #[test]
    fn next_for_app_skips_outbound() {
        let s = Store::open_in_memory().unwrap();
        let a = s.insert_inbound(&dm("one"), 1, None).unwrap();
        s.insert_outbound(
            "dm:010203040506",
            MsgKind::Direct,
            2,
            2,
            None,
            "out",
            "sent",
            None,
        )
        .unwrap();
        let c = s.insert_inbound(&dm("two"), 3, None).unwrap();
        assert_eq!(s.next_for_app(0, "x").unwrap().0, a);
        assert_eq!(s.next_for_app(a, "x").unwrap().0, c);
        assert!(s.next_for_app(c, "x").is_none());
        assert_eq!(s.messages("dm:010203040506", None, 10).len(), 3);
    }

    #[test]
    fn channel_sender_split() {
        let s = Store::open_in_memory().unwrap();
        let mut f = vec![resp::CHANNEL_MSG_RECV_V3, 0, 0, 0, 1, 2, 0];
        f.extend_from_slice(&7u32.to_le_bytes());
        f.extend_from_slice(b"Alice: hello: there");
        let id = s
            .insert_inbound(&protocol::parse_inbound(&f).unwrap(), 1, Some("Test"))
            .unwrap();
        let m = s.message(id).unwrap();
        assert_eq!(m.sender.as_deref(), Some("Alice"));
        assert_eq!(m.text, "hello: there");
        assert_eq!(m.conv, "ch:1");
    }
}
