# Privacy Policy — Arkx

Arkx is an **offline-first desktop application**. This policy is short because
there is almost nothing to disclose.

## Data collection: none

- Arkx collects **no personal data**, no usage statistics, no telemetry and no
  crash reports.
- Arkx makes **no network connections** — not for updates, not for anything.
  (GitHub release downloads happen in your browser, outside the app.)

## What stays on your device

- **Archives you open**: read locally, listed and extracted locally. File names
  from archives are shown in the UI and never leave your machine.
- **Passwords**: typed archive passwords live only in RAM for the extraction
  they unlock. They are never written to disk, logs or config files.
- **Settings**: Arkx stores no configuration file and no history. The file
  manager may remember that Arkx opens archives (standard MIME association).

## Permissions

Arkx needs only what the job requires:

- read the archives you select, write the files you ask to extract;
- spawn the bundled or system `7z` helper for formats the native backend
  does not cover.

No camera, microphone, location, contacts or background services are used.

## Questions

Open an issue: https://github.com/padelle2603/arkx/issues
