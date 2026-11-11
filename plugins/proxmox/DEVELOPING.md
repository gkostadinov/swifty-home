# Developing the Proxmox VE plugin

## Layout

| File | What it holds |
| --- | --- |
| `src/lib.rs` | `PLUGIN`, the manifest, and the pure calls: `validate`, `readiness`, `devices`, `refusal`, then `connect` and `setup` |
| `src/config.rs` | The typed config and its checks, and the grammar of a VM's local id (`vm<id>`, `<host id>-vm<id>`) |
| `src/protocol.rs` | Pure: the API paths, the power and console request bodies, and how each reply is read |
| `src/trust.rs` | The token header and which certificates each host may present |
| `src/driver.rs` | `Driver` and `MachineHost`: power, status, the VM list, charts, the console, further hosts |
| `src/setup.rs` | The guided setup |
| `tests/driver.rs` | The driver against the test kit's `MockProxmox` and `MockTlsPve` |
| `tests/proxmox_tls.rs` | The token and the console ticket reach only a server whose certificate checks out |
| `tests/setup.rs`, `tests/conformance.rs` | The setup, scripted; the conformance suite; refusals in dry-run against live |

The tests in `tests/driver.rs` and `tests/proxmox_tls.rs` keep the names they had
in the hub (`crates/swifty/tests/adapters.rs` and `proxmox_tls.rs`), so a test can be
followed across the move.

`tests/proxmox_tls.rs` starts its own TLS servers on loopback, because the test kit
has none with a chosen certificate name or a TLS websocket. They are test code. The
plugin's own code opens nothing except through `swifty-net`.

## Rules this plugin keeps

- **No site data in code.** No VM id, node name or host name appears in `src/`. VMs
  come from the hub's adopted devices, the node from the config.
- **A VM must be adopted before anything is sent to it.** `refusal` checks the
  instance's adopted devices, and the driver checks the hub's directory again
  before every call, so the two always give the same answer.
- **Never retried.** Every command is one request. The engine decides about retries.
- **The token and the console ticket stay secret.** The token header is marked
  sensitive. The ticket is a `Secret` from the moment it is read, and appears only
  in the websocket URL handed to `swifty_net::console_connect`.
- **The CA from the secret store.** `trust::client` hands the PEM from the `ca`
  slot to `swifty_net::Http::with_ca_pem` as it is; nothing is written to a file. A
  further host's CA is the file named for it in `host_files_dir`.

## Protocol sources

Everything this plugin says to Proxmox comes from Proxmox VE's own API
documentation: the API viewer that every Proxmox VE host serves under
`/pve-docs/api-viewer/`, and the administration guide's chapters on API tokens and
on user management and permissions. The calls used:

| Call | For |
| --- | --- |
| `GET /api2/json/version` | that the API answers, when the token may not read the node |
| `GET`, `POST /api2/json/nodes/{node}/status` | the node's uptime; `command=shutdown` and `command=reboot` |
| `GET /api2/json/nodes/{node}/rrddata`, `.../qemu/{vmid}/rrddata` | stored charts, `timeframe` and `cf=AVERAGE` |
| `GET /api2/json/nodes/{node}/qemu` | the VM list |
| `GET .../qemu/{vmid}/status/current` | a VM's status |
| `POST .../qemu/{vmid}/status/start`, `shutdown`, `reboot` | VM power |
| `POST .../qemu/{vmid}/vncproxy` with `websocket=1`, then `.../vncwebsocket` | the VNC console |
| An `Authorization` header of `PVEAPIToken=` and the token | how every call authenticates |

Proxmox VE is free software under the GNU AGPL, version 3, by Proxmox Server
Solutions GmbH. This plugin speaks its documented HTTP API and contains no Proxmox
code. No code, schema or table was copied from Proxmox or from any other project,
including client libraries for this API. The list of chart fields passed on
(`RRD_FIELDS`) names fields the API documents; it is this plugin's own choice of
which to keep.

The plugin carries no VNC (RFB) code: the hub relays the console's bytes unchanged.

For `NOTICE`: "The Proxmox VE plugin uses the documented Proxmox VE API. Proxmox is a
trademark of Proxmox Server Solutions GmbH."

## Testing

`cargo test -p swifty-plugin-proxmox` runs everything against loopback stand-ins. No
test reaches a real host.
