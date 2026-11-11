# Swifty Speech

A local speech-to-text service for Swifty Home. It turns a short push-to-talk
WAV clip into text. Audio and text stay in memory for the request and are never
written or logged. It has no authority over anything else.

This folder is all that is needed on the target machine.

## Install

1. From an **administrator** prompt, copy this folder to a protected location,
   for example `C:\Program Files\Swifty Speech`. Keep `model\` next to
   `swifty-speech.exe`.
2. In an **administrator** command prompt in that folder, run:

       swifty-speech.exe install

This registers the automatic-start Windows service `SwiftySpeech`, generates
the token if it does not exist yet, and starts the service. The model takes a
few seconds to load. Move the folder only after uninstalling, because the
service runs the executable from where it was installed.

**The location must be protected.** The service runs as SYSTEM, so anyone who
can change its files could take over the machine. `install` refuses to register
the service if the executable, its folder or the model files can be changed by
anyone other than Administrators, SYSTEM or TrustedInstaller. A folder directly
under `C:\`, in a user profile, or moved in from one (a move keeps the old
permissions, a copy does not) is refused. `C:\Program Files` is fine.

`install` also refuses an existing `%ProgramData%\SwiftyHome` folder that a
non-administrator could change. Restrict it, or delete it if nothing needs it.

## Upgrading from Glimdock Speech

This service used to be called `GlimdockSpeech` and kept its files in
`%ProgramData%\Glimdock\speech`. Copy the new folder to a protected location as
above and run `swifty-speech.exe install` from there. It:

1. stops the old `GlimdockSpeech` service, if it is there;
2. moves the token and the log from `%ProgramData%\Glimdock\speech` to
   `%ProgramData%\SwiftyHome\speech`, and removes the old folder once it is empty;
3. registers `SwiftySpeech` with the old service's options, removes the old
   service, and starts the new one.

The clients need no change: the token is the same, and so is the address. An old
service that was given no `--bind` listened on `127.0.0.1:8790`, and the new one
is registered with `--bind 127.0.0.1:8790` to keep it. Options given to
`install` replace the old service's options instead of adding to them.

`install` checks everything before it changes anything. It refuses an old
folder that a non-administrator could change, and a token in both folders: in
that case delete the one the clients do not use. If it stops part way, run it
again; the old service stays registered until the new one is. You can then
delete the old installation folder.

## Choose who can reach it

By default the service listens on `127.0.0.1:8791`, reachable from this machine
only. (The Swifty Home hub's own default is `127.0.0.1:8790`, so both can run on
one machine.) To accept a remote client, pass the address to listen on and the
clients to accept. These options are saved with the service:

    swifty-speech.exe install --bind 192.0.2.30:8791 --allow 192.0.2.10 --allow 192.0.2.11

`--allow` can be repeated. If it is present, connections from any other address
are refused with 403. `--threads N` sets the number of decoding threads
(default 4). `--token-file PATH` and `--model DIR` choose other locations. A
token file given to `install` must be readable only by Administrators and
SYSTEM, and is created that way if it does not exist. To change options,
uninstall and install again.

**The installer does not touch the firewall.** If a remote client needs the
service, allow the port yourself, for example:

    netsh advfirewall firewall add rule name="Swifty Speech" dir=in action=allow protocol=TCP localport=8791 remoteip=192.0.2.10,192.0.2.11

**Traffic is not encrypted.** The token, the audio and the transcript cross the
network in clear, the token does not expire, and `--allow` checks only the
source address. Anyone who can watch the traffic can read the audio and reuse
the token. Keep remote clients on a wired or isolated network segment, or carry
the traffic through a tunnel such as WireGuard.

## The token

Every request except `GET /healthz` needs `Authorization: Bearer TOKEN`.

The token is stored in `%ProgramData%\SwiftyHome\speech\token`, readable only by
Administrators and SYSTEM. It is kept across reinstalls. The program never
prints it. To read it, use an administrator prompt:

    type "%ProgramData%\SwiftyHome\speech\token"

To replace the token, for example if it may have been seen, delete the file and
restart the service in an administrator prompt, then give the new token to the
clients:

    del "%ProgramData%\SwiftyHome\speech\token"
    sc stop SwiftySpeech
    sc start SwiftySpeech

The service log is `%ProgramData%\SwiftyHome\speech\speech.log`. Each request
adds a line with a timestamp, status, duration and sizes. Start-up lines name
the model folder, the token file's path and the listening address. Refused and
timed-out connections are counted once a minute, and an internal failure adds
its source location. The log never holds audio, text or the token.

## Test it with curl

These lines are for the default bind, in an administrator command prompt on the
machine itself. With `--bind` and `--allow`, use the bound address instead of
`127.0.0.1:8791`, from a client on the allow list.

    curl http://127.0.0.1:8791/healthz

should print `{"status":"ok"}`. Then, with a WAV file:

    set /p TOKEN=<"%ProgramData%\SwiftyHome\speech\token"
    curl -H "Authorization: Bearer %TOKEN%" -H "Content-Type: audio/wav" --data-binary @clip.wav http://127.0.0.1:8791/v1/transcribe

The reply is JSON with `text`, `audio_ms` and `decode_ms`.

The clip must be a 16-bit PCM WAV file, mono or stereo, 8 kHz to 48 kHz, at most
30 seconds and at most 8 MiB. Other responses: 400 malformed WAV, 401 missing or
wrong token, 403 address not allowed, 408 body took more than 15 seconds,
413 body too large, 415 unsupported format, 422 audio too long or empty,
503 busy or still loading (see the `Retry-After` header), 500 internal failure.

To try a file without the service or any network:

    swifty-speech.exe transcribe clip.wav

## Uninstall

In an administrator prompt:

    swifty-speech.exe uninstall

This stops and removes the service. The token and log stay in
`%ProgramData%\SwiftyHome\speech`. Delete that folder if you no longer want
them, and remove any firewall rule you added.

## Running it in a console

`swifty-speech.exe run` runs in the foreground and logs to the console. As an
ordinary user, give it a token file of your own, which is created if missing:

    swifty-speech.exe run --token-file %USERPROFILE%\swifty-speech-token

An existing file is refused if anyone other than you, Administrators and SYSTEM
can read it.

Licences are listed in `LICENSE-NOTICES.md`.
