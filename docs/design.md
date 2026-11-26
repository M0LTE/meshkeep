<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# meshkeep design notes

Source of truth for the protocol is the MeshCore companion firmware (`examples/companion_radio/MyMesh.cpp`, `src/helpers/wifi/SerialWifiInterface.cpp`, `src/helpers/ArduinoSerialInterface.cpp`), checked against companion v1.18.0 (firmware version code 14).

## Shape

One process, a handful of tokio tasks:

- **radio** (`src/radio.rs`): connects to the radio over TCP or serial, reconnects with exponential backoff (1 s doubling to 30 s, reset after a session in which the radio said anything), and shuttles frames to and from the hub. Each session has an epoch number so frames from a dead session are ignored.
- **clients** (`src/clients.rs`): one task per app listener and per app connection. Pure framing, no protocol logic.
- **hub** (`src/hub.rs`): a single task that owns all protocol state: the command queue, the in-flight command, every app connection, and the sync cursors. Everything that matters is decided here, sequentially, so there are no locks around protocol state.
- **web** (`src/web.rs`): axum server with the embedded single-page app, a small JSON API and a WebSocket of hub events.
- **store** (`src/store.rs`): SQLite (bundled), WAL mode.

All time goes through the `Clock` trait (`src/clock.rs`). Tests use `FakeClock`, which only moves when told to, so timeouts, backoff and ack expiry are tested without real sleeps or elapsed-time assertions.

## Framing

Both directions use a 3 byte header: a marker byte, then a u16 little-endian length. App to radio uses `<` (0x3c), radio to app uses `>` (0x3e). TCP and serial use the same framing. Like the firmware's WiFi interface, meshkeep ignores zero-length frames from apps and skips frames longer than 176 bytes (`MAX_FRAME_SIZE`).

## Command serialisation

The radio handles one command at a time and replies in order, with no request ids. meshkeep therefore keeps exactly one command in flight to the radio. Every other command waits in a queue.

Replies are frames with codes below 0x80; pushes are 0x80 and above and can arrive at any time, including in the middle of a multi-frame reply. A reply goes to whoever sent the in-flight command; pushes go to every connected app.

To know when a command is finished, meshkeep has a table of what each known command answers (`protocol::expect_for`), taken from `handleCmdFrame`:

- most commands: one frame of a specific code, or `RESP_CODE_ERR`;
- `CMD_GET_CONTACTS`: `CONTACTS_START`, any number of `CONTACT`, then `END_OF_CONTACTS` (or `ERR`);
- `CMD_REBOOT "reboot"` and `CMD_FACTORY_RESET "reset"`: nothing (the radio restarts);
- self telemetry (`CMD_SEND_TELEMETRY_REQ` with no key): a `PUSH_CODE_TELEMETRY_RESPONSE`;
- unknown or future commands: forwarded unchanged, finished by the first non-push frame.

A frame of the wrong code for the in-flight command is dropped. This matters after a timeout: if the radio answers late, the stale answer is not handed to the next command's sender.

If the radio does not answer within 10 seconds the command is abandoned. The app gets nothing, exactly as if the radio had lost it; sending a late error would be mistaken for the answer to the app's next command. Two timeouts in a row restart the radio link.

Fairness: meshkeep's own commands (handshake, queue draining, contact refreshes) and app/web commands are in two queues served alternately, so a long drain does not starve apps.

## Talking to the radio

On every (re)connect meshkeep runs the app handshake: `CMD_DEVICE_QUERY` (announcing protocol version 3 or the highest an app has announced), the channel list (`CMD_GET_CHANNEL` for each slot the radio reports), `CMD_APP_START`, `CMD_SET_DEVICE_TIME` from the host clock, `CMD_GET_CONTACTS`, then a full drain.

The radio's offline queue is drained with `CMD_SYNC_NEXT_MESSAGE` until `NO_MORE_MESSAGES`:

- when the radio pushes `PUSH_CODE_MSG_WAITING` (this push is consumed, never forwarded);
- on every connect;
- every 60 seconds as a safety net.

The radio forgets a message the moment it hands it over, so draining is careful:

- A message is stored before the next one is requested. If SQLite refuses (disk full, permissions), meshkeep keeps the message in memory, stops draining, and retries every 5 seconds; the rest stay safe on the radio until then.
- A frame that does not parse is stored verbatim rather than dropped, and still handed to apps.
- Message frames (codes 7, 8, 16, 17, 27) only ever answer a sync, so they are stored whatever command is in flight, for example when a sync timed out and its answer turns up late.
- A sync is finished only by a message frame, `NO_MORE_MESSAGES`, or a code newer than this firmware knows (taken as a new kind of message and stored verbatim). A stray `OK` or `ERR` from a timed-out command cannot end a drain.

## Radio restarts

After forwarding a command that restarts the radio (`CMD_REBOOT "reboot"`, `CMD_FACTORY_RESET "reset"` once its `OK` arrives, or `CMD_RUN_CLI_COMMAND` with `reboot`, `poweroff` or `shutdown`), meshkeep closes the radio link and reconnects, which runs the handshake again. This matters for USB boards with a separate USB-UART chip, where the serial port survives the reboot and the link would otherwise carry on with a radio that has forgotten its session settings. The CLI forms get no reply from the firmware, so meshkeep does not wait for one.

If the radio restarts on its own, the first message drained afterwards arrives in the legacy layout (it has forgotten that meshkeep asked for v3). meshkeep then repeats the session setup (`CMD_DEVICE_QUERY`, `CMD_APP_START`, `CMD_SET_DEVICE_TIME`) before continuing.

meshkeep caches self info, device info, contacts and channels in SQLite. It keeps the cache current by watching replies to anyone's commands, `NEW_ADVERT`, `CONTACT_DELETED`, and by refetching a contact after `PUSH_CODE_ADVERT` or `PATH_UPDATED`, or after an app adds, updates or removes contacts or channels.

## Message format per app

The firmware formats queued messages according to the protocol version the app announced with `CMD_DEVICE_QUERY`: version 3 and up get `CONTACT_MSG_RECV_V3`/`CHANNEL_MSG_RECV_V3` (with SNR), older get the legacy codes. meshkeep always asks the radio for v3 and stores v3 frames. Each app connection remembers its own announced version and gets frames converted back to the legacy layout if it asked for less than 3. When forwarding an app's `CMD_DEVICE_QUERY` to the radio, meshkeep raises the version byte to at least 3 so an old app cannot downgrade what the radio sends.

## Per-device sync

This is the hard part: the companion protocol has no client identity. The radio's own queue is destructive (a message given to one app is gone) and it only ever has one app.

### Identity

A **client** is a name with its own cursor (the id of the last message it is known to have received).

- Default: each listener is one client. Port 5000 is "phone", port 5001 is "tablet". This is explicit, survives IP address changes, VPNs and NAT, and needs nothing from the app.
- `per_address = true`: each remote IP address on that listener is its own client (`phone@192.168.1.20`). Convenient when devices have fixed addresses; wrong behind NAT or with private, rotating addresses.

Rejected alternatives: the app name in `CMD_APP_START` (the same for every copy of the app), and the TCP connection itself (apps reconnect all the time, which is the whole problem).

### Delivery

`CMD_SYNC_NEXT_MESSAGE` from an app is answered from the store, never forwarded: the next stored incoming message after that client's cursor, or `NO_MORE_MESSAGES`.

The cursor only advances when delivery is confirmed. The app sends commands one at a time and waits for each reply, so when the same connection sends *any* further frame, the message it was last handed has arrived. Then the cursor moves past it. If the connection drops first, the message stays undelivered and is handed out again on the next sync.

Trade-off: delivery is at least once. A duplicate needs a disconnect in the short gap between meshkeep writing a message and the app asking for the next one. The alternative, advancing on write, can silently lose a message whenever a phone's connection dies with data in flight, which is exactly the situation meshkeep exists for.

When a message is stored, every connected app gets a `PUSH_CODE_MSG_WAITING`, just as the radio sends one per queued message, so apps sync promptly.

### Where a new client starts

- A configured listener's cursor is created when meshkeep starts with it in the config, at the current end of the store. From then on nothing is missed, even if the device has never connected.
- A device first seen on a `per_address` listener starts at the later of the listener's creation point and the 100th most recent message, so a new device does not receive years of history.

The web interface is not a sync client; it reads the store directly and shows everything.

## Sending

Sends from apps are forwarded unchanged. meshkeep also records them for the web interface: `CMD_SEND_TXT_MSG` + `RESP_CODE_SENT` gives the expected ack CRC and timeout; a later `PUSH_CODE_SEND_CONFIRMED` with that CRC marks the message delivered. An app's retry (same timestamp and text) updates the same record. Channel sends are recorded on `RESP_CODE_OK`.

Sends from the web go through the same queue as app commands. Status runs `sending`, `sent`, then `delivered` on ack, `unconfirmed` if no ack arrives within the radio's estimate plus 5 seconds (a later ack still turns it to delivered), or `failed` if the radio refuses. The web makes one attempt; it does not retry or reset paths the way the app can.

Limits follow the firmware: 160 bytes of text for a DM; for a channel, 160 minus the node name and ": ".

## Sent messages in other apps

The companion protocol has no frame for "a message was sent from somewhere else", so a message sent from the web or from one app never shows up in the other apps. With `mirror_sent` on (the default), meshkeep fakes it: once the radio has accepted a plain text send (`RESP_CODE_SENT` for a DM, `RESP_CODE_OK` for a channel), it stores a mirror frame that goes out through the normal sync path to every client except the one that sent it (the web is never a sync client, so a web send reaches all apps).

- Channel message: a received channel message on that channel index, with the text the firmware actually puts on air, `<own node name>: <text>` (cut to 160 bytes as the firmware does), so the app shows it under your own name.
- DM to contact X: a received DM from X's key prefix with the text `You: <text>`. The app files it in X's conversation as if X had written it; the prefix makes clear it is yours.
- Timestamp is the one in the send command, SNR 0, `path_len` 0xFF (the firmware's value for a message that was not flooded, shown as direct).
- Layout follows each app's announced version (v3 or legacy), as for received messages.
- Only plain text is mirrored: not CLI or command text, signed, raw or binary sends, and not refused sends. An app retry of the same DM is not mirrored twice.

Mirrors are stored as their own rows (direction `mirror`, with the sending client's key) and never appear in the web view, which shows the real sent message.

## Repeats of sent channel messages

The app shows a sent channel message as heard repeated once the radio hears repeaters pass it on. meshkeep does the same for the web view, for channel messages sent from the web and from apps.

The firmware pushes every packet it receives as `PUSH_CODE_LOG_RX_DATA` (SNR, RSSI, raw packet), duplicates included and with nothing to enable; apps still get these pushes unchanged. For each one meshkeep reads the packet (`Packet::readFrom`: header, optional transport codes, path, payload). If it is group text (`PAYLOAD_TYPE_GRP_TXT`) for a channel it knows, it checks the 2 byte HMAC-SHA256 and decrypts it with the channel secret (AES-128-ECB, `Utils::MACThenDecrypt`). The secrets come from `RESP_CODE_CHANNEL_INFO`. A decrypted message whose timestamp and channel match one of our sends from the last 30 minutes, and whose text starts with our node name, is a repeat; the exact text decides between two sends in the same second. Every copy heard counts, as in the app, so the number grows with each repeater that is heard.

The web shows "Heard 1 repeat" or "Heard N repeats" in place of "Sent", updated live. DMs have delivery acks instead and are not counted.

## Contact types and rooms

The web sidebar groups contacts by the type in the contact record: chat nodes under Direct, room servers under Rooms. Repeaters and sensors are not chat targets and only appear, under Repeaters, once a conversation with them exists.

Room servers forward posts as signed DMs (`TXT_TYPE_SIGNED_PLAIN`): the first 4 bytes of the text are the author's key prefix. meshkeep stores the prefix as the sender and the web shows the author's name when the prefix matches a contact, otherwise the hex prefix. A room server only forwards posts to clients that are logged in to it; logging in to rooms from the web is not supported yet, so do that from an app.

## Radio away

Apps stay connected when the radio link drops. While it is down:

- answered from the store: `CMD_SYNC_NEXT_MESSAGE`, `CMD_DEVICE_QUERY`, `CMD_APP_START`, `CMD_GET_CONTACTS` (with the `since` filter), `CMD_GET_CONTACT_BY_KEY`, `CMD_GET_CHANNEL`, and `CMD_GET_DEVICE_TIME` (host clock);
- `CMD_SET_DEVICE_TIME` gets `OK` (meshkeep sets the radio clock itself on reconnect);
- anything else waits up to 3 seconds for the radio, then gets `RESP_CODE_ERR` with `ERR_CODE_BAD_STATE` (4), the firmware's code for "cannot do that now";
- a command in flight when the link drops gets the same error.

So the app's connect sequence succeeds and stored messages sync even with the radio off.

## Shared radio state

Some firmware state is per session but the radio only knows one session, meshkeep's:

- `CMD_SET_FLOOD_SCOPE_KEY` (send scope) is shared by every app.
- The signing sequence (`CMD_SIGN_START`/`DATA`/`FINISH`) keeps one buffer; two apps signing at once would interfere.
- Pending login, status, telemetry and binary requests are matched by the radio, and the result pushes go to every app.

These are rare in practice and are left as is.

## Web authentication

One password from the config. A successful sign-in creates a random 256-bit session token, stored in SQLite (so restarts keep you signed in) and sent as an `HttpOnly`, `SameSite=Strict` cookie valid for 30 days. Sign-in attempts are handled one at a time across all clients, and a wrong password holds that slot for one second, so guessing is limited to one try a second in total.

Cross-site protection, for every request:

- `POST` requests and the WebSocket are refused when the browser's `Origin` header does not match the `Host` the request was made to. Requests without `Origin` (non-browser clients) are allowed.
- With sign-in off, the `Host` must be an IP address, `localhost`, a single-label name (`pi`), or end in `.local`, `.lan`, `.home.arpa` or `.internal`. This blocks DNS rebinding, where a hostile site points its own domain at your LAN address. If you reach meshkeep by another name, turn sign-in on.

Serve it behind a TLS reverse proxy if it is reachable from outside your network.

## Testing

`src/sim.rs` is a simulated companion radio written from `handleCmdFrame`: offline queue with the firmware's eviction rule, v3/legacy message layouts by announced version, multi-frame contact listing, ack table, reboot, and the WiFi interface's "new client replaces old" rule. It can hold commands unanswered to test serialisation and timeouts.

Integration tests (`tests/`) run the simulator, meshkeep and fake apps over real local TCP sockets on a fake clock. `cargo run --example sim_radio` runs the simulator on its own for trying the web interface without hardware.
