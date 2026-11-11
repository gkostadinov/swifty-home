# TLS and the reverse proxy

Passkeys only work on a secure address: `https://`, or `http://localhost` on the
machine itself. That is the one real hurdle for a new hub, so here it is plainly.

Swifty Home itself speaks plain HTTP on `127.0.0.1:8790` and never listens on the
network. A reverse proxy in front of it holds the certificate and passes requests
through. Choose one of four ways.

| You have | Use |
| --- | --- |
| just this machine, for now | [1. localhost](#1-localhost-nothing-to-set-up) |
| a domain name you own | [2. Caddy with a public certificate](#2-a-domain-you-own-caddy) |
| only your home network | [3. Caddy with its own authority](#3-home-network-only-caddy-with-tls-internal) |
| nginx or another proxy already | [4. Your own proxy](#4-your-own-proxy-nginx) |

Whichever you pick, the address in the browser must be **exactly** the `origin` in
`/etc/swifty-home/swifty.toml`, and `rp_id` is that name without `https://`:

```toml
[webauthn]
origin  = "https://home.example.org"
rp_id   = "home.example.org"
rp_name = "Swifty Home"
```

`swifty init --origin https://home.example.org` writes this for you. Passkeys are
bound to the name, so changing it later means everyone adds their passkeys again.

## 1. localhost: nothing to set up

With the origin `http://localhost:8790`, open that address in a browser on the hub's
own machine. Good for a first look and for a hub on your desktop. A phone cannot use
it.

## 2. A domain you own: Caddy

Point a name such as `home.example.org` at the machine (a DNS record), and let ports
80 and 443 reach it. Then install Caddy and use `packaging/caddy/Caddyfile`, with
`home.example.org` replaced by your name. Caddy fetches and renews the certificate
by itself.

```sh
sudo cp packaging/caddy/Caddyfile /etc/caddy/Caddyfile
sudo systemctl reload caddy
```

## 3. Home network only: Caddy with `tls internal`

No public DNS, no open ports. Use the same Caddyfile and uncomment `tls internal`.
Caddy then signs the certificate with its own small authority, and each phone and
computer has to trust that authority once:

1. Copy Caddy's root certificate, `pki/authorities/local/root.crt` under Caddy's data
   directory, to the device. It is public, not a secret.
2. **iPhone and iPad:** open the file and install the profile (Settings, General,
   VPN & Device Management), then turn on full trust (Settings, General, About,
   Certificate Trust Settings).
3. **Android:** Settings, Security, Encryption & credentials, Install a certificate,
   CA certificate.
4. **Windows, macOS, Linux:** add it to the system's trusted root authorities.

The name must also resolve to the hub on your network: a DNS entry on your router,
or a hosts file entry.

## 4. Your own proxy: nginx

`packaging/nginx/swifty-home.conf` is a generic template. Replace the name and the
two certificate paths, then:

```sh
sudo nginx -t
sudo systemctl reload nginx
```

Whatever proxy you use, it must:

- pass `Host` and `Origin` through unchanged (the hub refuses a request whose
  `Origin` is not exactly the configured one);
- not buffer `/api/v1/events`, the live event stream;
- pass the WebSocket upgrade on `/api/v1/console/connect`, for the VM console;
- allow request bodies up to 9 MB on `/api/v1/voice`, for voice clips;
- ideally rate-limit `/api/v1/auth/`.

## Check it

```sh
sudo swifty doctor
```

`swifty doctor` reaches the hub through its public origin and checks that the proxy
answers, sends the hub's security headers through, and does not buffer events.
