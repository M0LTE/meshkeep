<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# meshkeep

meshkeep keeps your MeshCore companion radio connected all the time, stores every message it receives, and lets several devices use it at once.

A companion radio talks to one app at a time and only holds a handful of messages while no app is connected. Phones drop that connection constantly. meshkeep sits between the radio and your devices:

- It stays connected to the radio over WiFi (TCP) or USB, and pulls messages off it the moment they arrive.
- It looks like a WiFi companion radio to the MeshCore app, so your phone and tablet connect to meshkeep instead, at the same time, and each catches up on everything it missed.
- It has its own web interface for reading and sending messages from any browser.

Works with MeshCore companion radios, following the companion protocol as of firmware v1.18.

## Install

On Debian, Ubuntu or Raspberry Pi OS (amd64, arm64 and 32-bit armhf):

```
curl -fsSL https://packet-net.github.io/apt/pubkey.asc | sudo gpg --dearmor -o /usr/share/keyrings/packet-net.gpg
echo "deb [signed-by=/usr/share/keyrings/packet-net.gpg] https://packet-net.github.io/apt ./" | sudo tee /etc/apt/sources.list.d/packet-net.list
sudo apt update
sudo apt install meshkeep
```

The package does not start meshkeep until you have configured it.

## Configure

Edit `/etc/meshkeep/meshkeep.toml`. At minimum, say where the radio is and set a web password:

```toml
radio = "192.168.1.50"       # WiFi companion (port 5000 assumed)
# radio = "/dev/ttyACM0"     # or a USB companion

[[listener]]
name = "phone"
bind = "0.0.0.0:5000"

[web]
bind = "0.0.0.0:8080"
password = "something long"
```

Then start it:

```
sudo systemctl enable --now meshkeep
journalctl -u meshkeep -f
```

Messages are kept in `/var/lib/meshkeep`.

Settings can also be given on the command line (`meshkeep --help`): `--radio`, `--listen name=host:port`, `--web`, `--password` (or `MESHKEEP_PASSWORD`), `--no-auth`, `--data-dir`.

## Connect the MeshCore app

In the app, add a WiFi/TCP connection to the meshkeep machine's address and the listener's port (5000 above). The app behaves as if it were talking to the radio directly.

Give each device its own listener so each keeps its own place in the message history:

```toml
[[listener]]
name = "phone"
bind = "0.0.0.0:5000"

[[listener]]
name = "tablet"
bind = "0.0.0.0:5001"
```

If you would rather use one port for several devices with fixed addresses, set `per_address = true` on the listener; meshkeep then tracks each connecting IP address separately.

## Web interface

Open `http://<machine>:8080`. It works on phones, tablets and desktops: channels, direct conversations and rooms on the left, full history, sending, and delivery status (delivered for DMs, repeats heard for channel messages). Every open browser updates live.

Sign-in uses the single password from the config. On a network you fully trust you can set `auth = false` under `[web]` instead; then the interface only answers when reached by IP address or a local name (such as `pi`, `meshkeep.local` or `meshkeep.lan`).

## Limitations

- The app has no way to be told about a message sent from another device. meshkeep works around this by showing it to the other apps as if it had been received: a channel message appears under your own node name, a DM appears in that contact's conversation as `You: ...`. Set `mirror_sent = false` to turn this off.
- A message that was being handed to an app at the moment its connection dropped is delivered again next time, so a rare duplicate is possible. Nothing is lost.
- Two devices sharing one listener (without `per_address`) share one place in the history, so each sees only some of the messages. Use one listener per device.
- A device first seen on a `per_address` listener gets at most the 100 most recent messages from before it connected.
- Some radio settings are per session in the firmware (flood scope, message signing). With several apps connected they share them.
- Replies to requests such as repeater logins and status queries go to every connected app, as the radio sends them without saying who asked.
- Room servers only forward posts to logged-in clients, and the web interface cannot log in to rooms yet; log in from an app.
- While the radio is unreachable, apps can still connect, sync stored messages and read contacts and channels; anything that needs the radio fails after 3 seconds.

## Docker

```
docker build -t meshkeep .
docker run -d --name meshkeep -p 5000:5000 -p 8080:8080 -v meshkeep:/data \
  -e MESHKEEP_PASSWORD=something-long meshkeep --radio 192.168.1.50 --listen phone=0.0.0.0:5000
```

For a USB radio add `--device /dev/ttyACM0` and `--radio /dev/ttyACM0`.

## More

Design notes, including how per-device sync works: [docs/design.md](docs/design.md).

## Licence

AGPL-3.0-or-later. See [LICENSE](LICENSE).

MeshCore is referred to here only to describe compatibility; meshkeep is an independent project.
