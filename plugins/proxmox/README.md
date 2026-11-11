# Proxmox VE plugin

`swifty-plugin-proxmox` connects Swifty Home to one Proxmox VE node through its HTTP
API, with an API token that has only the privileges it needs. It stays on the local
network.

## What it controls

| Device | Local id (and the target it asks for) | Class | What it can do |
| --- | --- | --- | --- |
| The node itself | `host` | `host` | Shut down, reboot, uptime, stored charts |
| A VM on the node | `vm<id>`, such as `vm101` | `vm` | Start, shut down, reboot, status, stored charts, and the VNC console when the config lists it |
| A VM on a further host | `<host id>-vm<id>`, such as `two-vm200` | `vm` | The same, except node power |

VMs are found with a scan and wait as **pending** until someone approves each one
with a passkey. Nothing can be sent to a VM that has not been approved.

Never done, by design:

- **Force-stop.** Only start, shutdown and reboot are sent, so a guest can always
  shut down cleanly.
- **A shell on the node.** `vncshell` and `termproxy` are not spoken. The console is
  only ever `qemu/<id>/vncproxy` for an approved VM on the console list.
- **Powering a further host.** Only the configured node can be shut down or rebooted.

## Setup

The guided setup (`swifty plugin add proxmox`, or the app) goes:

1. **Create an API token.** In the Proxmox web interface, under Datacenter,
   Permissions, create a user for Swifty Home and an API token for it with privilege
   separation. Give the token a role with these privileges and no others:

   | Privilege | On | For |
   | --- | --- | --- |
   | `VM.Audit` | the VMs | the VM list, status and charts |
   | `VM.PowerMgmt` | the VMs | start, shutdown, reboot |
   | `VM.Console` | the VMs that get a console | the VNC console |
   | `Sys.Audit` | the node | the node's uptime and charts (without it, the node only shows that the API answers) |
   | `Sys.PowerMgmt` | the node | shutting down and rebooting the node |

   Proxmox shows the token's value once. Copy it as `user@realm!tokenid=uuid`.
2. **Connect.** The API address (`https://` and the host's address with port 8006),
   the node name, the token, and the CA certificate that signed the host's
   certificate (on the host: `/etc/pve/pve-root-ca.pem`). Tick "publicly trusted"
   only when the host's certificate comes from a public authority.
3. **Approve VMs.** The setup lists the VMs on the node (templates are left out) and
   reads each one you tick once, so a missing privilege shows at once.
4. **Consoles.** Pick which of those VMs may have a VNC console.

The setup needs a live hub: it reads the host. When it is run again and the stored
token is kept, it saves the new settings without asking the host.

## Config

| Key | Required | Default | Meaning |
| --- | --- | --- | --- |
| `url` | yes | | The API's base URL: `https://`, the host's address and port, nothing after it. Plain `http` only on loopback, so the token never crosses the network in clear |
| `node` | yes | | The node's name as Proxmox shows it: letters, digits and dashes |
| `public_ca` | no | `false` | The host's certificate chains to a public root, so no CA is stored |
| `console_targets` | no | none | Rows of `{ vm = "<local id>" }`: the VMs that may have a VNC console. No other VM ever gets one |
| `host_files_dir` | no | none | An absolute path: the only directory a further host's token and CA files may come from. Adding a further host is refused until it is set |

For example, in a site file:

```toml
[[instance]]
id = "proxmox"
plugin = "proxmox"
[instance.config]
url = "https://192.0.2.10:8006"
node = "pve"
console_targets = [{ vm = "vm101" }, { vm = "vm102" }]
host_files_dir = "/var/lib/swifty-home/proxmox-hosts"
```

## Secret slots

| Slot | Required | Holds |
| --- | --- | --- |
| `token` | yes | `user@realm!tokenid=uuid` |
| `ca` | unless `public_ca` is set or `url` is loopback `http` | The PEM of the CA that signed the host's certificate |

The instance shows "needs setup" until the slots it needs hold a value.

## Trust

- The configured host's certificate must chain to a public root or to the CA in the
  `ca` slot, and must name the host in `url`. Anything else fails the handshake, and
  the token is never sent.
- A further host is added by address, so it trusts **only** the CA file named for
  it, never the public roots.
- The VNC websocket uses the same trust as the API calls of the same host.
- The console ticket Proxmox hands out travels only in the websocket request to the
  host that issued it, after its certificate has checked out. It is held as a secret,
  so it never appears in a debug line, an error or a log.

## Further hosts

The hub's `proxmox.add_host` action adds another Proxmox node by IP address. Its
token and CA live in two files directly inside `host_files_dir`, readable by the
service alone. Its VMs are scanned with the others and get local ids
`<host id>-vm<id>`. One further host being down does not hide the VMs of the others.

## Coming from a version 1 config

| Version 1 | This plugin |
| --- | --- |
| `[devices.proxmox] url`, `node` | `url`, `node` |
| `[devices.proxmox] token_file` | the `token` slot, holding the file's content |
| `[devices.proxmox] ca_file` | the `ca` slot, holding the file's content; with no `ca_file`, `public_ca = true` |
| `[console] targets = ["vm101"]` | `console_targets = [{ vm = "vm101" }]` |
| `[machines] secrets_dir` | `host_files_dir` |
| `[machines] allowed_host_networks` | stays with the hub, as `[network] allowed_host_networks` |

## Limits

- One node per instance. A cluster's other nodes are further hosts, or instances of
  their own.
- Charts are Proxmox's own averages for the last hour, day or week.
- A VM whose VNC display shows only the emulated VGA, such as one with its GPU passed
  through, shows nothing useful in the console. Leave it off `console_targets`.
- Containers (LXC) are not offered.
