# Glimdock plugin

`swifty-plugin-glimdock` connects Swifty Home to a **Glimdock node**. [Glimdock](https://github.com/gkostadinov/glimdock) is a
self-hosted monitor with a small desk display; its node is a small HTTP
service on a machine that collects that machine's telemetry (CPU, memory, power,
temperatures, disks, storage pools, Bluetooth) and pairs its own monitoring agents.

The plugin is read-only. It does two things and nothing else:

- it reads the node's **snapshot**, which feeds the charts, the health page and
  spoken questions such as "how hot is the CPU";
- it asks the node to **start pairing an agent** when the owner runs `agent.pair`.

Any node that keeps the HTTP contract below works with it, so this README is also
the specification for writing one.

## The device

Each instance is one node and offers one device:

| | |
| --- | --- |
| Local id | `collector` (also hinted as its target, so an existing install keeps the target `collector`; a second node gets `<instance>-collector`) |
| Class | `telemetry` (owner-only by default) |
| Capabilities | `Metrics` (the snapshot), `AgentPairing` (`agent.pair`) |
| Default name | Glimdock node |
| State | `{ "kind": "telemetry", "data": <the snapshot> }` |

Every operation is refused: a node takes no commands. Reading is `state.read`, and
pairing is the hub's own `agent.pair` action, which is critical and needs a passkey
step-up. The plugin declares no custom actions.

## Setup

The guided setup is one form, then one test read:

1. **Form**: node URL, snapshot path, access token, and optionally the agent pairing path.
2. **Test read**: when the hub is live, the flow reads the snapshot once with the
   token you entered. If that fails it says why and offers the form again.
   In dry-run nothing is contacted and the instance is saved untested.
3. **Done**: the config is stored, the token goes into the secret slot `token`.

Setting a node up again pre-fills the form. Leave the token empty to keep the stored
one; the flow then cannot test the node, because it never sees a stored secret.
The pairing endpoint is never called during setup.

## Config

| Key | Required | What |
| --- | --- | --- |
| `url` | yes | The node's base URL: scheme (`http` or `https`), host and port, nothing else. No path, user, query or fragment. Example: `http://192.0.2.10:8080` |
| `snapshot_path` | yes | Path of the snapshot endpoint, such as `/api/snapshot` |
| `pairing_path` | no | Path of the agent pairing endpoint. Without it, `agent.pair` fails with "pairing_path is not configured" |

Both paths must be plain absolute paths: they start with one `/`, and contain no
`//` at the start, `..`, `?`, `#`, `\` or control characters, and are at most 256
characters. A path can never change the host the token goes to; the driver checks
that again on every request.

Example instance config:

```json
{ "url": "http://192.0.2.10:8080", "snapshot_path": "/api/snapshot", "pairing_path": "/api/agents/pair" }
```

## Secret slots

| Slot | Required | What |
| --- | --- | --- |
| `token` | yes | The node's reader token. Sent only as `Authorization: Bearer <token>`, never in a URL, never logged, never in an error. Until it is stored the instance reads "needs setup" |

## The HTTP contract

Every request the plugin makes:

- goes to the host and port of `url`, on the path from the config, with no query string;
- carries `Authorization: Bearer <token>` and `Accept: application/json`;
- is sent once. There are no retries and no redirects. The hub's connect and read
  timeouts apply (`[network]` in `swifty.toml`);
- uses a fresh connection.

Answers:

| Status | Meaning to the hub |
| --- | --- |
| `2xx` with a JSON body | Success |
| `2xx` with a body that is not JSON | Refusal: "the device sent an unreadable reply" |
| `401`, `403` | Refusal: "the Glimdock node did not accept the token" |
| anything else | Refusal: "the Glimdock node refused with HTTP <status>" |
| no answer in time | Silence (`Timeout`). Never read as a value |
| connection refused | Silence (`Unreachable`) |

A body larger than 8 MiB is refused.

### Snapshot: `GET <snapshot_path>`

The answer is one JSON object. **Every field is optional.** A field that is missing,
`null` or not a number is shown as "not reporting", never as zero, so a node only
sends what it really measured.

The hub reads these fields today. Anything else is kept in the device's state and
ignored by the charts and the health page.

**Single readings** (numbers), charted over time:

| Field | Unit |
| --- | --- |
| `cpu_usage` | percent |
| `memory_used_bytes` | bytes |
| `pkg_watts` | watts, CPU package |
| `pkg_temp_c`, `cpu_temp_c` | degrees Celsius |
| `gpu_temp_c`, `gpu_watts` | degrees Celsius, watts |
| `wall_watts` | watts at the wall |
| `energy_kwh` | kilowatt-hours since the node's energy counter began |
| `energy_watts` | watts, everything the energy counter covers |

**Per core**: `cores` (or `per_core`), an array. Each entry is a number (usage in
percent) or an object with any of `usage`, `util_pct`, `temp_c`, `mhz`, `watts`.

**Nested form.** A node may nest its readings instead. The plugin adds the flat name
when the nested value is a number and the flat field is not already there:

| Nested | Becomes |
| --- | --- |
| `host.cpu_pct` | `cpu_usage` |
| `host.mem_used_bytes` | `memory_used_bytes` |
| `host.cpu_cores` (array) | `cores` |
| `power.package_w` | `pkg_watts` |
| `power.cpu_temp_c` | `pkg_temp_c` and `cpu_temp_c` |
| `gpus[]`: the first entry with `"kind": "discrete"`, its `temp_c` and `power_w` | `gpu_temp_c`, `gpu_watts` |
| `energy.total_kwh` | `energy_kwh` |
| `energy.domains[]`: the sum of each `watts` that is a number (the first 8 domains); none, no reading | `energy_watts` |

**Threads, computers and energy.** Three more groups of fields are optional and are
passed through untouched (their text is made safe like all text). The host card draws
them: the load on each thread grouped by the computer pinned to it, the host's memory
split by computer, and the energy used with what it cost. A field a node cannot read is
absent or `null`; the page never reads that as zero or guesses it.

| Field | Meaning |
| --- | --- |
| `host.cpu_core_kinds` | One entry for each entry of `host.cpu_cores`, in the same order (the logical CPU number is the index): `"performance"`, `"efficiency"` or `null`. Absent on a machine that is not hybrid |
| `host.mem_total_bytes` | The host's memory, for the memory split |
| `guests[]` | The virtual machines and containers, each with `id` (the VM number, a whole number), `name`, `type` (`"vm"` or `"lxc"`), `status` (`"running"` when it is), `cpu_count`, and the memory fields below |
| `guests[].cpu_affinity` | The host's logical CPU numbers the guest is pinned to, as a sorted list. `null` when it may run on any thread. Absent when its configuration could not be read, which says nothing about where it runs |
| `guests[].mem_host_bytes`, `mem_used_bytes`, `mem_assigned_bytes` (or `mem_total_bytes`) | The host memory the guest takes (its share of the bar), what it says it uses, and what it was given. The share falls back to `mem_used_bytes` |
| `energy` | The node's energy counter: `since` (unix seconds, when it began), `updated_at`, `total_kwh` (the sum of the domains), `domains[]` (at most 8, each with `id`, `label`, `kwh`, `source`, `watts`) and `scope` |
| `energy.scope` | `"components"` when the counter covers CPU and GPU only, `"wall"` when it is measured at the wall |

A guest is shown under the name the hub gives the Proxmox VM with the same VM number
when it has one, and under the node's own name otherwise.

```json
{
  "host": { "cpu_cores": [10.0, 20.0, 5.5, 7.0], "cpu_core_kinds": ["performance", "performance", "efficiency", "efficiency"], "mem_used_bytes": 8589934592, "mem_total_bytes": 17179869184 },
  "guests": [
    { "id": 100, "name": "media", "type": "vm", "status": "running", "cpu_affinity": [0, 1], "cpu_count": 2, "mem_host_bytes": 3221225472, "mem_used_bytes": 2147483648, "mem_assigned_bytes": 4294967296 },
    { "id": 101, "name": "files", "type": "vm", "status": "running", "cpu_affinity": null, "cpu_count": 2 }
  ],
  "energy": {
    "since": 1794380000, "updated_at": 1794381234, "total_kwh": 12.3456,
    "domains": [{ "id": "cpu-package", "label": "CPU package", "kwh": 12.0, "source": "rapl", "watts": 48.2 }],
    "scope": "components"
  }
}
```

**The cost of the energy.** The node counts kilowatt-hours; the hub prices them. The
owner sets one price (an amount per kWh and a currency code), and each time the hub reads
the node it charges what `energy.total_kwh` added since the last read at the price in force.
A counter that restarted (a different `since`, or a total that went down) is followed by its
new total. Only `energy.since` and `energy.total_kwh` are needed for this; a node that sends
no `total_kwh` has no cost. The user guide says more.

**Sources.** `sources` is an object that says which parts of the snapshot are live.
The health page believes a part only when its source says it is reporting; without
an entry, that part is "not reporting".

| Part | Source names |
| --- | --- |
| `faults` | `faults`, `kernel`, `journal`, `dmesg` |
| `disks` | `disks`, `smart`, `smartctl` |
| `storage` | `storage`, `zfs`, `zpool`, `truenas` |
| `bluetooth` | `bluetooth`, `bt` |
| the temperature and power readings | `cpu`, `sensors`, `hwmon`, `rapl` (if one of these is present and stopped, no readings are charted from that snapshot) |

An entry reports when it is `true`; a string `ok`, `up`, `reporting`, `fresh`,
`healthy`, `online` or `running`; or an object that is not `"stale": true`, whose
`age_s` (or `age_seconds`) is at most 600, and whose `reporting`, `ok`, `up` or
`available` flag is true, or whose `status` or `state` is one of the words above.

**Health sections:**

| Field | Shape |
| --- | --- |
| `faults` | object of counters (numbers), such as `segfault_count_24h` |
| `disks` | array, or object keyed by name; each with `name` (or `device`), `health` (text such as `PASSED`, or a boolean), `media_errors` |
| `storage` | totals `read_errors`, `write_errors`, `checksum_errors`, `scrub_errors` (also accepted: `read`, `write`, `checksum`, `cksum`, `scrub`), and `pools`: array or object of pools, each with `name` (or `pool`) and the same counters |
| `bluetooth` | `controllers.drops_24h`, `headsets.drops_24h` |

Example:

```json
{
  "host": { "cpu_pct": 12.5, "mem_used_bytes": 8589934592, "cpu_cores": [10.0, 20.0, 5.5, 7.0] },
  "power": { "package_w": 45.0, "cpu_temp_c": 61.0 },
  "gpus": [{ "kind": "discrete", "temp_c": 52.0, "power_w": 110.0 }],
  "faults": { "segfault_count_24h": 0 },
  "disks": [{ "name": "nvme0", "health": "PASSED", "media_errors": 0 }],
  "storage": { "read_errors": 0, "write_errors": 0, "checksum_errors": 0, "pools": [{ "name": "tank", "read_errors": 0 }] },
  "sources": { "cpu": "ok", "faults": { "reporting": true, "age_s": 30 }, "disks": true, "storage": "ok" }
}
```

### Agent pairing: `POST <pairing_path>`

Body: `{"name": "<the name the owner gave the agent>"}`, as `application/json`.

A `2xx` JSON answer is handed back to the owner as it is, for the person installing
the agent: for example `{ "code": "PAIR-1234", "expires_in": 600 }`. The hub does not
write it to the audit log. The node decides what pairing means; the hub only starts
it. An agent only ever reports readings: nothing in Swifty Home can send a command
to an agent or a node.

Pairing is sent only by a live hub, only for `agent.pair` after a passkey step-up,
and never during setup.

## Limits

- Read-only. No command reaches a node, and the plugin has no custom actions.
- One device per node. Several nodes are several instances.
- The snapshot is read on demand and by the hub's sampler (about once a minute, for the
  charts and the energy cost); the plugin keeps no cache.
- A node that needs a client certificate or a private CA is not supported yet.
- `Metrics` and `AgentPairing` are always offered; without `pairing_path`, pairing
  fails with a reason instead of being hidden.

See `DEVELOPING.md` for where the protocol comes from and how the crate is tested.
