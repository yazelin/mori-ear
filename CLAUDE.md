# mori-ear — agent guide

For Claude / agents working in this repo. README.md is the human-facing entry point; this file is what an agent needs to navigate efficiently.

## What this is

A minimal Rust CLI: global hotkey → microphone capture → Groq Whisper STT → Groq LLM cleanup → clipboard + Ctrl+V paste-back into the focused window. No GUI, no tray, no persistent state. The "ear" organ in the Mori universe — runs as an independent process so mori-desktop restarts don't kill voice input.

## Layout

```
src/
  main.rs        — entry, config, hotkey thread, paste-back
  audio.rs       — cpal capture + WAV encode
  stt.rs         — Groq Whisper multipart POST
  cleanup.rs     — Groq LLM繁中 cleanup
scripts/
  install-autostart.ps1  — Windows scheduled task (self-elevating)
  remove-autostart.ps1   — delegates to install -Remove
  install-autostart.sh   — Linux XDG autostart entry
  restart.sh             — dev hot-reload: build → kill → relaunch
.github/workflows/
  build.yml      — Linux + Windows release builds; tag v* publishes a Release
```

## Build / run

```sh
cargo build                       # dev
cargo build --release             # release (Windows: windows_subsystem = "windows", no console)
cargo install --path .            # install to ~/.cargo/bin

# Linux dev loop
bash scripts/restart.sh           # build debug + relaunch (logs to /tmp/mori-ear.{out,err})
bash scripts/restart.sh --release # cargo install --path . then relaunch ~/.cargo/bin/mori-ear

# Windows: same cargo install --path .; no auto-restart helper script (kill manually in PowerShell)
```

No test suite yet. End-to-end verification is "real human + real microphone" — see README usage section.

## Platform-specific gotchas (have bitten us)

### Windows: global-hotkey requires a manual Win32 message pump

`global-hotkey 0.6` on Windows binds `RegisterHotKey` to a hidden tool window but does **not** spawn an internal message pump. The caller must run `GetMessage` / `DispatchMessage` on the same thread that created the manager — `tokio::time::sleep` does NOT pump Win32 messages. Symptom of forgetting this: `register` reports success, the "ready" log prints, but hotkey events never reach `GlobalHotKeyEvent::receiver()`.

`spawn_hotkey_thread` in `src/main.rs` is the fix: dedicated OS thread owns the manager, registers the hotkey, then loops on `GetMessageW`. Linux X11 is unaffected because the crate's X11 impl spawns its own event thread.

If you ever change the hotkey loop, do NOT collapse it back onto the tokio runtime.

### Windows: `windows_subsystem = "windows"` in release builds

Release builds drop the console subsystem (eliminates the black flash at scheduled-task logon). `AttachConsole(ATTACH_PARENT_PROCESS)` reattaches to a terminal if launched manually so logs still surface; the call silently no-ops for autostart. Don't remove either of those two pieces in isolation — they're a pair.

### Windows: install-autostart.ps1 self-elevates

`Register-ScheduledTask` / `Unregister-ScheduledTask` under `\` root require admin. The script detects non-elevated invocation and re-launches itself via `Start-Process -Verb RunAs`, passing `-OriginalUser` so the registered principal is the **calling** user (not whatever admin account UAC may pick to run the elevated shell). If you touch this script, preserve the OriginalUser passing — the alternative is registering the task for the wrong account on managed machines.

### Linux: single-instance + restart timing

X11 `XGrabKey` is per-client. Two mori-ear instances → second silently fails to grab. `single-instance` crate (abstract Unix socket on Linux, named mutex on Windows) ensures one process. `scripts/restart.sh` uses `pkill -9` + a poll loop because the abstract socket needs a beat after process death to be released.

### Linux: paste-back terminal detection

`paste-back_linux_clipboard` walks `xdotool getactivewindow getwindowpid` → `/proc/<pid>/comm` to decide between `Ctrl+V` and `Ctrl+Shift+V`. Same idea on Windows via `GetForegroundWindow` + `QueryFullProcessImageNameW`. If you add a terminal emulator, append to the lookup table in `needs_shift_for_paste*`.

## Config loading

Order (partial merge):

1. `~/.mori/ear.json` — full overrides (anything set wins)
2. `~/.mori/config.json` `providers.groq.api_key` — fallback for `groq_api_key` when ear.json doesn't set it (shared with mori-desktop)
3. `GROQ_API_KEY` env var — final fallback

Old behavior was "ear.json wins entirely or nothing" — a user writing a one-line ear.json to override hotkey lost the groq key and the process died. Don't regress this; the partial-merge logic in `Config::load` is small but load-bearing.

## Audio guardrails (why STT might be skipped)

`handle_event` (Released branch) bails on:

- `duration_secs < 0.25` — hotkey press too brief, treated as a misfire
- `rms_db < -45.0` — recording is essentially silence, would trigger Whisper hallucinations ("謝謝", "請訂閱", etc.)

Tune in `src/main.rs` constants `MIN_DURATION` / `MIN_RMS_DB`. The thresholds intentionally trade off some "very quiet whisper" sensitivity for not pasting YouTube end-credit hallucinations.

## CI / release

- `build.yml` runs on every push + PR for both targets. Tag `v*` triggers the `release` job that publishes a GitHub Release with both artifacts.
- `FORCE_JAVASCRIPT_ACTIONS_TO_NODE24: "true"` is set so the v4 `actions/checkout` + `actions/upload-artifact` survive GitHub's Node 20 removal (2026-09-16). Annotation still appears until those actions get bumped to v5.

## What to NOT do

- Do not add a tray icon / GUI to mori-ear. That's mori-desktop's job.
- Do not add a feature toggle for "skip cleanup" to ear.json beyond the existing `raw` flag — the cleanup LLM is the difference between "謝謝你訂閱頻道" hallucinations and clean繁中.
- Do not introduce a tokio task that polls `GlobalHotKeyEvent::receiver()` on a tokio worker on Windows. See gotcha above.
- Do not auto-update `~/.mori/config.json` from mori-ear. That file is mori-desktop's source of truth; mori-ear is a read-only consumer of `providers.groq.api_key`.

## Linked organs

- [mori-desktop](https://github.com/yazelin/mori-desktop) — body / GUI / chat panel / annuli. Shared config dir, otherwise independent.
- Future: `mori-eye` (vision / screenshots), `mori-hand` (OS automation), `mori-lip` (TTS).
