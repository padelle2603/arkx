# Privacy Policy — Arkx

Arkx is an **offline-first desktop application**. This policy is short because
there is almost nothing to disclose.

## Data collection: none

- Arkx collects **no personal data**, no usage statistics, no telemetry and no
  crash reports.
- Arkx makes **exactly one kind of network connection**, described below. There
  is no other.

## The one network request

When the GUI starts, Arkx sends a single `HEAD` request to

```
https://github.com/padelle2603/arkx/releases/latest
```

to find out whether a newer release exists. If one does, a small "Arkx *n.m.p*
is available" window appears; closing it, or pressing **Not now**, changes
nothing. **Download** simply opens the release page in your browser.

What that request does **not** contain:

- no account, no token, no API key, no cookie jar;
- no GitHub API call, so it does not spend the API rate limit that other apps
  on your IP share;
- no version, distribution, hostname or machine identifier — the User-Agent is
  the single word `arkx`;
- no request body and no response body: GitHub answers with an empty redirect
  that carries the release tag in a header;
- nothing is written to disk as a result, and no timestamp is kept.

What GitHub necessarily sees, like any website you visit: your IP address and
the request time. If you would rather it did not, run Arkx offline, or set
`--check-update` aside and take releases from the browser yourself.

`arkx --check-update` runs the same request on demand and is scriptable: exit
`0` means you are current, `10` means a newer release exists, `1` means GitHub
was unreachable **or the repository has no published release yet**.

## What stays on your device

- **Archives you open**: read locally, listed and extracted locally. File names
  from archives are shown in the UI and never leave your machine.
- **Passwords**: typed archive passwords live only in RAM for the extraction
  they unlock. They are never written to disk, logs or config files.
- **Settings**: Arkx stores one configuration file, `config.json` in
  `$XDG_CONFIG_HOME/arkx/` (`~/.config/arkx/config.json` by default). It holds
  only the two speed profiles you pick — one of three levels for extraction,
  one of three for compression. No personal data, no history. The file manager
  may remember that Arkx opens archives (standard MIME association).

## Permissions

Arkx needs only what the job requires:

- read the archives you select, write the files you ask to extract;
- spawn the bundled or system `7z` helper for formats the native backend
  does not cover.

No camera, microphone, location, contacts or background services are used.

## Questions

Open an issue: https://github.com/padelle2603/arkx/issues
