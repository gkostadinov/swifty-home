# LG webOS TV plugin (`swifty-plugin-webos`)

Controls an LG TV with webOS on the local network over SSAP, the TV's JSON
protocol on a websocket, and turns it on with Wake-on-LAN. No cloud and no LG
account are involved.

## What it controls

One device of class `tv`, with the local id and target `tv`.

| Action | What is sent | Read back from |
| --- | --- | --- |
| `tv.power` off | `ssap://system/turnOff` | `getPowerState` |
| `tv.power` on | one Wake-on-LAN magic packet (needs `wol_mac`) | `getPowerState` |
| `tv.input` | `ssap://system.launcher/launch` with the input's app | `getForegroundAppInfo` |
| `tv.key` | one button frame on the TV's pointer socket | nothing can show it; the run ends "sent" |

Inputs: HDMI 1 to 4 and Live TV. Keys: up, down, left, right, ok, back, home,
exit, volume up and down, mute, channel up and down, play and pause.

A TV that does not answer is an error, never a reading of "off": an off TV and a
hung or unplugged one look the same from here.

## Setup and pairing

`swifty plugin add webos`, or the web wizard. It needs the hub in live mode.

1. **Form**: the TV's address, and its MAC address for Wake-on-LAN.
2. **Certificate**, for `wss` only: the hub connects once, sends nothing but the
   TLS handshake, and shows the SHA-256 fingerprint of the certificate the TV
   presented. Trust it, and from then on only that certificate is accepted from
   the TV (trust on first use). Setting the TV up again skips this step while it
   still presents the certificate stored for it, and warns when it does not.
3. **Wait**: the hub connects and registers without a key. The TV shows a prompt
   asking whether to allow the connection; accept it with the remote within a
   minute.
4. **Done**: the TV hands out a client key. The hub reads the TV once with it and
   stores it in the secret slot `client_key`. The key is never shown.

Only the setup ever starts pairing. The driver only presents the stored key. If
the TV does not accept it (after a factory reset, say), the TV offers a prompt
instead; the driver hangs up at once, stops connecting and reports the TV as down
until the setup is run again. A background poll never makes the first
connection, so a stale key can raise at most one prompt, and only because
someone asked for something on the TV.

## Config

```json
{ "url": "wss://192.0.2.40:3001", "cert_sha256": "<64 hex digits>",
  "wol_mac": "02:00:00:00:00:01", "wol_broadcast": "192.0.2.255:9" }
```

| Field | Required | Meaning |
| --- | --- | --- |
| `url` | yes | `ws://<address>:3000`, or `wss://<address>:3001`. A scheme, host and port, nothing else. Newer TVs only offer `wss` |
| `cert_sha256` | for `wss`, and only there | The SHA-256 of the TV's certificate, in hex. The TV's certificate is self-signed and names no address, so it is accepted by this pin and nothing else. The TLS signature is still checked |
| `wol_mac` | no | The TV's MAC address. Without it the TV can be turned off but not on, and turning it on is refused |
| `wol_broadcast` | no, `255.255.255.255:9` | Where the wake packet goes, as `IP:port` |

## Secret slots

| Slot | Required | Meaning |
| --- | --- | --- |
| `client_key` | yes | The key the TV handed out when its prompt was accepted. Stored by the setup. Until it is set the TV is listed, disabled, with "not paired yet" |

## Limits

- The certificate is trusted on first use: the setup cannot tell the TV from
  something else answering at its address, so confirm the fingerprint against the
  TV's (or set it up on a network you trust).
- Wake-on-LAN needs the TV's "turn on via Wi-Fi/LAN" setting, and a broadcast
  that reaches the TV's network.
- The pointer socket the TV reports for key presses is only followed on the TV's
  own host and ports: the scheme, host, port and pin always come from the config.
- Apps other than the inputs above cannot be launched; a running app is shown
  by its id.
- Each call opens its own connection with the hub's timeouts and is never retried
  by the plugin.
