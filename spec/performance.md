# Performance

CayenChat must stay lightweight, frugal with memory and fast. It is a compact,
traditional IRC client that users leave running all day; typing, IME
composition and channel switching must stay responsive however long it runs
and however much traffic arrives. This document records how performance is
measured, the current baseline, the existing measures that must be kept, and
what to compare when multi-server connections and image display are added.

Targets marked **proposal** are not agreed yet. Do not treat them as
requirements until the project owner accepts them.

## Constraints for upcoming work

These follow from the requirement and bound the next features (multi-server,
image previews, a shared display layer, IRCv3, icons and experimental
extensions):

- More connections must not add periodic polling; stay event driven.
- Keep log virtualization and the cached panes.
- Logs, diagnostics, queues and image caches are bounded per server *and*
  across the application.
- Image fetching and decoding never block the UI thread. Image caches are
  budgeted in decoded bytes and account for GPU-side textures.
- Icon display and preview display are separate ordinary settings. When off,
  the log returns to traditional IRC density and no image is fetched or
  decoded.
- IRC URL expansion covers images only; no general web page previews.
- Matrix, if added, should be able to show media events and server-provided
  previews through the same display and cache layer. Do not abstract all of
  Matrix now.
- Draft IRCv3 extensions are separate experimental options, off by default,
  independent of the display settings.

## Existing performance measures (keep these)

Do not build a second mechanism for any of these; extend them instead.

| Measure | Where | Why |
| --- | --- | --- |
| One dedicated `cayenchat-irc` thread with a current-thread Tokio runtime per connection | `irc-core::Connection::connect` | Socket, TLS and parsing never run on the UI thread (D006). |
| Bounded queues: 512 events worker→UI, 128 commands UI→worker | `irc-core` `EVENT_CAPACITY`, `COMMAND_CAPACITY` | A slow UI back-pressures the worker, which stops reading the socket, instead of buffering without limit. |
| Event pump awaits events; no timer polling. One pump task per connection, up to 256 events per update, yielding between batches | `ui` `spawn_event_pump`, `EVENT_BATCH_LIMIT` | An idle connection wakes nothing; input, redraws and other servers interleave with bursts. The earlier 50 ms poll capped throughput and woke 20 times a second. |
| No periodic timers while idle | registration progress/timeout timers stop at 001; the watchdog ends once connected; menu-bar hover polling runs only while the bar is hover-revealed | Idle CPU and wakeups stay near zero. The `irc` crate still sends its own PING every 180 s per connection. |
| Per-channel log cap: 2,000 lines, trimmed to 1,000 when exceeded; server log the same | `app` `push_bounded` | Memory per log is bounded; trimming in chunks amortizes the shift. |
| Diagnostics transcript: newest 1,000 lines per server in a `VecDeque` | `ui::session` `DIAGNOSTIC_LIMIT` | While the transcript is on every IRC line is recorded, so it must be bounded and O(1) to trim. |
| IRC lines reach the transcript only during registration or while the debug transcript is on | `irc-core` `Transcript`, `Connection::set_transcript` | An established session does no per-line formatting, allocation or extra event for the transcript, which halves the worker→UI events of incoming traffic. |
| Combined subwindow: newest 1,000 lines across other channels, rebuilt only when the last message sequence or the selection changes, by merging conversation tails newest-first | `ui` `SUB_LOG_LIMIT`, `sync_log_lists`, `newest_lines` | Keystrokes do not rebuild it, and the rebuild does not grow with every retained line when there are many conversations. |
| At most 1,000 conversations per network; messages for unknown channels go to the bounded server log | `app` `MAX_CONVERSATIONS_PER_NETWORK` | A hostile server or bouncer cannot grow memory without limit. |
| WHOIS collection capped (32 pending nicknames, 512 items each); rosters cached only for joined channels | `irc-core` `MAX_PENDING_WHOIS`, `MAX_WHOIS_ITEMS` | Same. |
| Virtualized panes: both logs, user list and channel tree use GPUI `list`/`uniform_list` with 400 px overdraw | `ui::log_list` | Only rows near the viewport are laid out. |
| `LogList::sync` splices only rows whose sequences appeared or disappeared | `ui::log_list` | Measured row heights and scroll position survive appends, trims and channel switches. |
| The four panes are separate views drawn with `AnyView::cached` | `ui` `ChatPane`, `ChatPanes` | A keystroke redraws only the window shell and the draft; a GPUI test (`typing_reuses_panes_and_new_messages_redraw_them`) asserts it. State changes that affect panes must `cx.notify()` the chat window. |
| Message times stored as minutes (`TimeOfDay`); a retained `Message` is 64 bytes plus its sender and text | `model` | Smaller logs, one fewer allocation per line. |
| QUIT/NICK republish rosters only for channels that contained the user; roster sort keys computed once per member | `irc-core` `RosterTracker`, `app` `sorted_members` | Large channels. |
| Linux: bounded per-word shaping cache in the vendored GPUI (swept every 128 lines, entries unused for four sweeps dropped) | `vendor/gpui`, see `PATCHES.md` | A channel switch does not reshape every newly visible line from scratch (issue #5). |
| Image previews are requested only by main-log rows being drawn; one application-wide `PreviewCache` bounds loads, queue, records and decoded bytes; loads run on GPUI's background executor | `media::cache`, `ui::previews` | Receiving or retaining image links costs nothing until a row is on screen; nothing exists while previews are off (no timer, thread or HTTP agent). |
| A pending preview reserves its full height; rows whose height changes after loading get only their own measured height replaced | `ui::previews`, `LogList::invalidate` | Images arriving do not move the scroll anchor, reset the list or relayout the history. |
| Avatars are requested only by drawn main-log message rows and member rows, from their own small cache; the slot has a fixed size below the line height; image fetches in flight are shared with previews | `ui::avatars`, `media::cache` | A roster or history full of avatars costs nothing until rows are shown; an arriving avatar never changes a row's height; both features on do not add up their fetch concurrency. Nothing exists while avatars are off. |
| Avatar references live once per network in `app::avatars`, keyed by user, not in messages | `app::avatars` | `model::Message` stays 64 bytes plus sender and text. |

## Resource bounds today

| Resource | Bound | Scope | Notes |
| --- | --- | --- | --- |
| Channel log | 2,000 messages, trimmed to 1,000 | per conversation | No application-wide bound: 1,000 conversations × 2,000 lines is allowed. Older history pages fill it up to 2,000 and never trim it. |
| Server log | 2,000 messages, trimmed to 1,000 | per network | |
| Conversations | 1,000 | per network | |
| Diagnostics transcript | 1,000 lines | per server | IRC lines are formatted and sent to the UI only during registration and while the debug transcript is on. No application-wide bound. |
| Combined subwindow | 1,000 rows (indices only) | window | Rebuilt by a heap merge of conversation tails that stops at 1,000 rows. |
| Received IRC line | 16 KiB including the line ending | per connection | Enforced by the vendored irc-proto codec while reading; a longer line ends the connection. Also bounds the text of every message. |
| Worker→UI events | 512 | per connection | Back-pressure, not a drop. |
| UI→worker commands | 128 | per connection | `try_send`; a full queue rejects the command. |
| WHOIS collection | 32 nicknames × 512 items | per connection | |
| Rosters | none | per channel | Kept four times: `irc`'s channel lists, `irc-core`'s `RosterTracker`, its `PresenceIndex` (see "Presence index"), and `app`'s sorted `members`. |
| Per-selection UI state | none | per visited server/channel | `main_lists` (one `LogList` with measured heights) and one `TextInput` entity per conversation; cleared only when a connection is applied from settings (automatic reconnects keep them). |
| Attachment | 32 MiB | one upload at a time | Upload only. |
| Preview loads in flight | 2 | application | Fetch plus decode; jobs started before previews were switched off still count until they return. |
| Queued preview requests | 16, newest served first, oldest dropped | application | Requests come only from drawn rows; a dropped request is made again when its row is drawn. |
| Preview records | 256 (ready, failed, queued and loading) | application | Least recently used ready or failed records go first; up to 4 waiting rows each. |
| Ready thumbnails | 32 MiB charged, least recently used evicted | application | Each thumbnail is charged twice its BGRA bytes (the `RenderImage` copy and its GPU atlas tile); at the largest size (400×200) that is 640,000 bytes, so about 52 thumbnails. Evicted ones are removed from the atlas. |
| Preview response | 8 MiB, 3 redirects, 5 s connect, 15 s total | per load | At most 16 MiB of response buffers at once (two loads). |
| Preview decoding | one at a time; ≤ 8192 px a side, ≤ 16.7 MP, decoder allocation ≤ 48 MiB | process | Checked from the header before decoding; the full image is freed once the thumbnail exists. |
| Preview retries | transient failures (network, 408/429/5xx) once more after 5 min; others never | per record | While the record is retained; redraws never retry. |
| Avatar metadata (irc-core) | 2,048 users with an avatar; values ≤ 2,048 bytes; 16 deferred channel syncs, 3 per channel | per connection | Only the `avatar` key is kept; later users get none. Reset on reconnect or lost capability. |
| Avatar lookups for later joiners (irc-core) | 64 pending, 8 unanswered, 2 sent per second after a 2 s pause, 2 attempts, 30 s timeout | per connection | Only live JOINs of users sharing no other channel; never NAMES, redraws or history. Further joiners are skipped until the table drains. |
| Own avatar requests (irc-core, app) | 1 outstanding, 20 s timeout, URL ≤ 400 bytes | per connection | Only on Send to / Remove from IRC Server, or after an avatar image upload. |
| Avatar directory (app) | 2,048 current + 512 retired entries | per network | Worst case about 6 MiB per network at the maximum URL length; typical URLs are ~100 bytes. Removed with the server. |
| Default avatars (ui) | 512 looks, one 32×32 SVG image each | application | Kept per look while avatars are shown (about 4 KiB of pixels each in GPUI's image cache and atlas); cleared when the setting is turned off. |
| Avatar loads | 2 in flight; with previews at most 3 media fetches in flight together | application | Decoding stays one at a time in the process (shared with previews). |
| Queued avatar requests | 32, newest first | application | Only from drawn rows. |
| Avatar records | 256 (ready, failed, queued, loading) | application | Least recently used ready or failed first. |
| Ready avatars | 2 MiB charged (32×32 BGRA × 2 for CPU copy and GPU tile = 8 KiB each) | application | Separate from the 32 MiB preview budget. Released images leave the GPU atlas after both avatar panes redrew. |
| Avatar response and decode | 2 MiB, 3 redirects, 5 s connect, 10 s total; ≤ 4096 px a side, ≤ 4.2 MP, decoder ≤ 24 MiB | per load | Centered square cropped and scaled to 32×32. |
| Linux shaping cache | swept every 128 lines | process | Age-based, not a byte bound. |

## Measuring

Everything runs against local fixtures. Never point the tools at a public
server or use real credentials.

### Tools

- `scripts/perf/irc_load_server.py` — a minimal plaintext IRC server on
  127.0.0.1 (standard library only). It registers the client, answers JOINs
  with a NAMES roster, and on its control port runs `history N` (N lines per
  channel at full speed) and `flood RATE SECONDS` (round-robin PRIVMSG; rate
  0 means as fast as the client reads). Each phase ends with a PING marker;
  the time to its PONG is the *drain lag*. During floods it also records a
  PING round trip every second. The client's IRC worker only reads the socket
  while the 512-event queue has room, so these reflect how far event handling
  fell behind. **They are not the time until a line appears on screen.**
- `scripts/perf/sample_process.py` — samples one process every second: RSS and
  CPU time (`ps` on macOS, `/proc` on Linux), and on macOS thread count, idle
  wakeups and `phys_footprint` (`top`'s MEM). CPU is reported as percent of
  one core over the window (above 100 % means more than one busy thread).
- `scripts/perf/window_visibility.swift` (macOS) — reports whether part of the
  app's window is on screen. GPUI stops its display link, and therefore
  drawing, for a fully occluded window, so a hidden window makes a load look
  cheaper than it is.
- `scripts/perf/run_baseline.py` — builds nothing; it launches
  `target/release/cayenchat` with an isolated `HOME` and `XDG_CONFIG_HOME`
  (a temporary settings file, local-file credential backend, no passwords),
  starts the fixture on free ports and runs the scenarios below. Results go
  to JSON; `--summarize FILE` prints medians and ranges.
- `crates/ui/src/perf_baseline.rs` — an ignored GPUI test that times typing,
  channel switching and event batches in the headless test platform.

### Procedure

```sh
cargo build --release --locked -p cayenchat-ui
python3 scripts/perf/run_baseline.py --runs 3 --out target/perf/baseline.json
cargo test --release --locked -p cayenchat-ui perf_baseline -- --ignored --nocapture
```

Repeat the UI test three times. During `run_baseline.py` a CayenChat window
opens on the current Space: keep it at least partly visible, leave the machine
alone, and quit other heavy work. Results where `window visible` is below 1.0
(any sample with the window hidden) are not comparable for the load
scenarios. `--load-only` runs just S2–S4b (about three minutes per run) for a
session in which someone keeps the window visible. Record the machine, OS,
compiler, commit and whether the build was clean (`run_baseline.py` stores
these; `git_dirty` also counts uncommitted script changes).

### Scenarios and load

Defaults: 10 channels (`#perf00`–`#perf09`), 50 NAMES entries each, 10 s
settling, 30 s sampling windows. Message bodies rotate among short ASCII,
long ASCII, Japanese, mixed text with a URL and a ~250-byte Japanese line.

| ID | State | Load |
| --- | --- | --- |
| S1 | Started, not connected; the settings window is open at startup | none |
| S2 | Registered, 10 channels joined | none |
| S3 | 2,000 lines per channel retained (the cap) | `history 2000`, then idle |
| S4a | Busy channels | 200 lines/s across all channels for 30 s |
| S4b | Overload | as fast as the client reads, 30 s |
| S5 | After logs and diagnostics were saturated | idle |
| S6 | After a second 30 s overload | idle (plateau check) |

Input and channel switching are timed by the UI test, not the runner: 10
channels with 50 members, 2,000 incoming lines per channel (each with its wire
diagnostic, so logs and the transcript are saturated), then 300 single
characters typed, 200 channel selections through `ChatWindow::dispatch`
(the path shortcuts use), 100 batches of 256 events (128 lines), and 300
more characters.

## Baseline (2026-09-27)

### Conditions

- Apple M2 Pro (12 cores), 32 GB, macOS 26.6.2; a 2560-point-wide main
  display and a 1920-point-wide second display; refresh rates were not
  recorded.
- rustc 1.95.0 (Homebrew), `cargo build --release --locked -p cayenchat-ui`
  at `0f4cb68` (unmodified application source; binary 12.7 MB). The default
  960×632 chat window, light theme, English UI.
- Plaintext fixture on 127.0.0.1, default scenario parameters above, three
  runs. Other applications were running and the machine was in use, so the
  window was not always visible (see below).

### Resources (quantitative, three runs)

Medians with the range across runs. CPU is percent of one core over a 30 s
window; memory is the last sample of the window. Idle scenarios do not draw,
so window visibility does not affect them.

| Scenario | CPU % | Idle wakeups/s | Threads (max) | phys_footprint MiB | RSS MiB |
| --- | --- | --- | --- | --- | --- |
| S1 unconnected (chat + settings windows) | 0.03 (0.00–0.03) | 0.00 | 5–8 | 86 (83–86) | 96 (82–108) |
| S2 connected, 10 channels | 0.13 (0.10–0.27) | 0.07 (0.07–0.10) | 6–8 | 58 (58–58) | 80 (79–92) |
| S3 20,000 lines retained | 0.24 (0.00–0.28) | 0.00 (0.00–0.14) | 7 | 69 (63–69) | 85 (80–93) |
| S5 after saturation | 0.30 (0.27–1.29) | 0.07 (0.00–0.17) | 8–10 | 75 (70–76) | 89 (89–105) |
| S6 after a second overload | 0.23 (0.00–1.70) | 0.10 (0.00–0.17) | 7–10 | 76 (71–76) | 95 (89–96) |

- Idle stays near zero CPU with at most about one wakeup per ten seconds;
  there is no periodic polling. The thread count varies between 5 and 10
  because AppKit, Metal and GPUI start and retire worker threads; the app
  itself adds one (`cayenchat-irc`) per connection.
- The settings window costs about 28 MiB of footprint (S1 versus S2), mostly
  its drawable surface. GPU-side memory therefore matters as much as heap.
- 20,000 retained lines plus a full transcript add 5–11 MiB (roughly
  250–550 bytes per retained line including layout state). After the logs and
  transcript are saturated, footprint stays at 70–82 MiB; a second overload
  changed it by −4 to +6 MiB (S5 → S6), so no sustained growth was seen
  within these short runs.

### Load (quantitative, split by window visibility)

GPUI on macOS stops drawing a fully hidden window. Each observation is
listed with whether the window was visible for the whole window
(`visible 1.0`) or hidden for most of it.

| Scenario | Window | CPU % | Lines/s read | PING RTT median / max | Drain lag |
| --- | --- | --- | --- | --- | --- |
| S4a 200 lines/s | visible (1 run) | 15.7 | 200 | 0.2 / 0.5 ms | 0.0 s |
| S4a 200 lines/s | mostly hidden (2 runs) | 1.5, 2.6 | 200 | 0.4–4.2 / 1.9–545 ms | ≤ 0.05 s |
| S4b maximum | visible (2 runs) | 59, 68 | 77,000, 115,000 | 67–184 / 1,660–1,850 ms | 0.9–1.3 s |
| S4b maximum | hidden (1 run) | 138 | 211,000 | 39 / 773 ms | 0.01 s |

- A steady 200 lines/s costs about 16 % of a core when drawn and 2–3 % when
  not: almost all of it is redrawing. Each incoming batch notifies the chat
  window, and all four cached panes re-render on the next frame.
- At the maximum rate the client stayed connected and back-pressure kept
  memory flat; worker-side lag reached about 1.8 s. The rate itself is not a
  stable metric on this machine: an earlier run without visibility
  recording varied 3–10× and was discarded, and a later short check
  (`--load-only`, 50 lines per channel, window visible) read 283,000 lines/s
  at 140 % CPU. Retained volume is not the explanation: the headless test
  gives nearly the same per-batch cost with 100 or 2,000 lines per channel
  (switch 1.93 versus 1.98 ms, batch 2.06 versus 2.00 ms). Likely factors are
  how much of the window is visible, which display it is on (refresh rate)
  and other load. Compare S4a CPU and the headless timings instead, and use
  S4b only as a stability check (stays connected, memory flat, lag bounded).
- A `sample` profile of a visible maximum flood (4 s, same binary) showed
  the main thread about 77 % busy: `Window::draw` about 21 % of samples
  (text layout about 4 %), `ChatWindow::handle_events` about 12 %. The
  `cayenchat-irc` thread was about 50 % busy, mostly in the `irc` codec,
  prefix parsing and UTF-8 decoding.

### Typing, switching and event batches (headless UI test, three runs)

10 channels × 2,000 retained lines (20,000), 1,000 diagnostics, 50 members
per channel. Microseconds; the three runs agreed within a few percent.

| Operation | Median | p95 | Max |
| --- | --- | --- | --- |
| Typing one character | 421–424 | 439–520 | 458–546 |
| Channel switch | 1,983–2,005 | 2,409–2,573 | 2,801–3,483 |
| 256-event batch (128 lines) | 2,001–2,094 | 2,083–2,685 | 2,768–3,215 |
| Typing after the batches | 431–432 | 454–537 | 554–644 |

Typing did not re-render any pane (`pane_renders` unchanged). These numbers
are app-side CPU per interaction on the test platform, which has a no-op text
system and no GPU: they exclude shaping, rasterization, presentation and
display latency.

### Limits of this baseline

Not measured or not confirmed:

- Keystroke-to-pixel and IME composition latency, and channel switching in
  the real window. No subjective GUI check was performed for this baseline.
- The load scenarios with the window visible in every run; only one or two
  visible observations exist, and the display refresh rate was not recorded.
- GPU memory separately from `phys_footprint`.
- Windows and Linux; TLS connections; sessions longer than about five
  minutes; channels with thousands of members and heavy JOIN/PART churn;
  private-message traffic (routed to the server log at the time; private
  conversations came later and are bounded at 100 per network).
- The 200 lines/s and maximum rates are synthetic. The fixture sends no
  JOIN/PART/QUIT churn during floods.

## Comparing multi-server support (next PR)

Run the same procedure on the parent commit and the branch on the same
machine, and add these:

- S2 with 1, 2 and 4 servers (same fixture on different ports, 10 channels
  each): threads, idle CPU and idle wakeups per added connection. Each
  connection currently adds one thread and a current-thread Tokio runtime; the
  `irc` crate's 180 s PING is the only idle timer. Idle wakeups must not grow
  with any polling.
- S3 with 4 servers × 10 channels × 2,000 lines: footprint growth per
  retained line, to check that per-line cost does not change.
- S4a on one server while others are idle, and on all servers at once:
  CPU, drain lag and PING round trip. Also check that one flooded server does
  not delay another's lines more than the shared UI thread requires.
- UI test extended to 4 servers × 10 channels: typing must still leave the
  panes cached (`pane_renders` unchanged) and switching across servers should
  cost about the same as within one.
- Combined subwindow rebuild with 40 conversations (done in the multi-server
  change; see below).
- Diagnostics: per-server transcripts must stay bounded and the app-wide total
  must be bounded too.

## Multi-server comparison (2026-09-27)

Same machine as the baseline, measured in one session against the parent
commit (`d0e2dd1`, PR #11) built in a separate worktree. The machine was busier
than during the baseline, so compare within this section only. Measurement
was kept short on purpose; the four-server process run was not completed.

Headless UI test, three runs each (medians, µs; 10 channels × 2,000 lines per
server):

| Build | Typing | Channel switch | 256-event batch |
| --- | --- | --- | --- |
| Parent, 1 server | 421–424 | 2,096–2,192 | 2,059–2,118 |
| Multi-server, 1 server | 426–434 | 1,842–1,873 | 1,875–1,947 |
| Multi-server, 4 servers (80,000 lines) | 447–454 | 1,715–1,875 | 2,026–2,344 |
| Multi-server before the combined-log merge, 4 servers | 458–469 | 2,971–3,034 | 3,198–3,455 |

Typing still re-renders no pane. The combined subwindow's collect-and-sort
made switching and batches grow with conversations (+33 % and +43 % at four
servers); merging tails newest-first removed that and also made one server
faster than the parent.

Process run, one server, three runs each (`run_baseline.py`; `top` now only at
window edges):

| Scenario | Parent footprint / CPU | Multi-server footprint / CPU |
| --- | --- | --- |
| S2 connected, idle | 58 MiB / 0.99 % | 58 MiB / 0.51 % |
| S3 20,000 lines, idle | 69 MiB / 0.44 % | 70 MiB / 0.38 % |
| S4a 200 lines/s | 75 MiB / 20.3 % | 75 MiB / 19.2 % |
| S6 after saturation | 70 MiB / 0.44 % | 73 MiB / 0.10 % |

Idle wakeups stayed at about one per second or less in both. Window
visibility again varied between runs (see the visible-runs column of
`--summarize`), so load CPU is indicative only.

Two servers (one short smoke run, 4 s windows, window visible): both
connected once and stayed connected; threads rose by two (one worker per
connection); footprint 65 MiB connected, 69 MiB with 500 lines per channel.
While the first server was flooded at the maximum rate (its PING round trip
about 1.2 s), the idle second server's PING round trip stayed at 0.2 ms
median, 1.6 ms max: one busy server does not delay another's lines.

Not measured: four servers in the process run, idle wakeups per added
connection over a full window, and the real window under multi-server load.

## Image previews (2026-09-27)

Inline previews (D018) are off by default. The limits in "Resource bounds
today" were chosen as follows:

- Two loads in flight and one decode at a time keep the worst transient
  memory near 2 × 8 MiB of responses plus one 48 MiB decode, while a screen
  of new images still fills within a few seconds.
- 8 MiB responses and 16.7 MP / 48 MiB decodes cover ImgBB screenshots and
  12 MP phone photos (4032×3024 JPEG decodes to about 35 MiB); larger
  images stay text links.
- A 200×100 logical preview box keeps rows compact; thumbnails are 400×200
  px at most so they stay sharp on 2× displays (320,000 BGRA bytes).
- The 32 MiB budget charges each thumbnail twice (CPU copy and GPU atlas
  tile), so about 52 large thumbnails: several screens of image-heavy log.
  Scrolling back past evicted images loads them again; there is no disk
  cache.
- 16 queued requests and 256 records bound bookkeeping for floods of links;
  retrying a transient failure once after 5 minutes avoids retry storms from
  redraws.

The vendored GPUI now frees the atlas space of a dropped image
(`vendor/gpui/PATCHES.md`); before, evicted thumbnails left dead space in
the 1024×1024 (4 MiB) atlas textures, which a single remaining tile or emoji
kept alive.

### Headless UI test (three runs, medians in µs)

Same machine and procedure as the baseline, measured alternately against the
parent `c214429` in one session. `scroll_20_rows` is new (the parent has no
such measurement). Previews on: every 20th line of each channel carries one
of 120 image links, served in memory as an 800×400 PNG; loads complete
between samples, so the timings are the UI-side cost.

| Build | Typing | Channel switch | 256-event batch | Scroll 20 rows |
| --- | --- | --- | --- | --- |
| Parent, 1 server | 422–452 | 1,810–2,114 | 1,959–2,126 | — |
| Previews off, 1 server | 424–469 | 1,837–2,141 | 1,975–2,046 | 1,514–1,585 |
| Previews on, 1 server | 375–385 | 1,729–1,752 | 1,828–2,167 | 1,420–1,435 |
| Parent, 4 servers | 451–453 | 1,840–1,960 | 2,079–2,209 | — |
| Previews off, 4 servers | 447–453 | 1,632–1,865 | 2,057–2,081 | 1,580–1,638 |

- Off matches the parent within run-to-run noise, and typing still
  re-renders no pane (asserted by the test, also with previews on).
- On is not slower: preview rows are taller, so fewer rows are laid out per
  screen. One on-run had a 18 ms maximum in the batch timing (p95 4.6 ms);
  the other two stayed below 2.8 ms.
- Previews-on bookkeeping at the end of the run, identical in all three runs:
  177 loads for about 1,000 retained image lines (only rows that were shown,
  including scrolling the whole 2,000-line log up and down and 200 channel
  switches), 128 requests dropped from the full queue while history arrived
  (the window drew each batch before any load ran), 125 evictions,
  52 records and 33,280,000 charged bytes at the end, below the 32 MiB
  (33,554,432) budget, nothing in flight or queued.

### Process (`run_baseline.py`, three runs each)

Binaries: parent `c214429`, this branch `318d8dd` (release, 13.1 → 16.4 MB:
the PNG/JPEG/GIF/WebP decoders and resizing are now linked), and the same
branch with `preview-fixture` for previews on (links every 20th line, 120
generated PNGs from 800×600 to 4032×3024 read from disk). Footprint is
`phys_footprint` in MiB, median [range].

| Scenario | Parent | Previews off | Previews on |
| --- | --- | --- | --- |
| S1 unconnected | 78 [78–100] | 79 [78–80] | 75 [75–76] |
| S2 connected, idle | 59 [58–59] | 59 [59–61] | 59 [58–63] |
| S3 20,000 lines, idle | 70 [69–70] | 70 [64–71] | 69 [51–73] |
| S5 after saturation | 72 [71–72] | 77 [73–78] | 110 [70–120] |
| S6 after a second overload | 72 [71–72] | 76 [73–77] | 111 [70–124] |

Idle CPU stayed at 0.2–0.9 % of a core and idle wakeups at about one per
second or less in all three; previews off added no thread (5–9 threads, as
before). The window was visible for only one or two parent runs and for
none of the branch runs (the machine was in use), so load-scenario CPU
(S4a/S4b) is not comparable and is not reported here; GPUI draws nothing
while the window is hidden, which also means fewer rows asked for images.

With previews on, footprint after the floods was 33–38 MiB above previews
off. That is within what the design allows but is process memory, not the
cache: the cache charges at most 32 MiB (16 MiB of thumbnails plus the same
again as the GPU atlas estimate), and `vmmap` on a GUI check showed about
40 MiB of `MALLOC_LARGE (empty)`: freed decode buffers that macOS's
allocator keeps for reuse. Footprint does not fall right after images are
freed, and it did not grow between S5 and S6.

A GUI check with the `preview-fixture` build and a visible window showed
thumbnails at the expected sizes, the outlined box while a 12 MP image
loaded, unchanged link text, text-only combined log, and the log following
new lines during a flood of image links.

Not measured: turning previews off in the running process (no UI automation
was available; the GPUI tests cover release of records, images and atlas
tiles), GPU memory separately from footprint, real network latency and TLS,
Windows and Linux.

## IRCv3 negotiation and server-time (2026-09-27)

A short check, not a baseline run. Same Apple Silicon Mac, release builds,
one run each of the ordinary headless workload
(`perf_baseline::perf_baseline --exact`: 10 channels × 2,000 lines, 1,000
diagnostics), parent `a27bd69` (master with PR #18) against the IRCv3 branch
after merging it. Medians in µs:

| Build | Typing | Channel switch | Scroll 20 rows | 256-event batch | Typing after |
| --- | --- | --- | --- | --- | --- |
| Parent | 427 | 1,854 | 1,524 | 1,990 | 436 |
| IRCv3 branch | 432 | 1,869 | 1,540 | 1,994 | 433 |

The differences are within run-to-run noise, so no further runs were made.
Typing still re-renders no pane (the test's `pane_renders` assertion). A
retained `model::Message` stays 64 bytes plus sender and text: the IRCv3
change adds no field (server-time reuses `TimeOfDay`), and PR #18's
`replayed` flag fits in existing padding. Negotiation runs inside each
connection's existing worker loop; no thread, timer or polling was added,
and with every option off the wire traffic is unchanged. The process-level
load scenarios were not rerun.

The later batch opt-in was not measured: it changes no UI code, no event
and no `model::Message` field (the existing `replayed` flag carries the
result). Per incoming line it adds at most a scan of the open history
references, and only for lines with a `batch` tag; the tracker holds at
most 64 references of at most 64 bytes per connection, stores nothing for
other batch types and never buffers messages. It runs in the existing
worker loop with no thread, timer or polling.

## User avatars (2026-09-27)

A short check, not a baseline run, in the Linux cloud container used for
this change (x86_64, 4 vCPUs shared, rustc 1.94.1; not comparable with the
macOS numbers above). Headless UI test in release mode, one run each,
medians in µs, 10 channels × 2,000 lines:

| Build | Typing | Channel switch | Scroll 20 rows | 256-event batch | Typing after |
| --- | --- | --- | --- | --- | --- |
| Parent `598d9dc` | 674 | 3,970 | 3,095 | 3,911 | 710 |
| Avatars branch, avatars off | 674 | 3,947 | 3,089 | 4,111 | 707 |
| Avatars branch, avatars on (`perf_baseline_avatars`) | 689 | 4,510 | 3,369 | 4,358 | 701 |

- Off matches the parent within run-to-run noise (an earlier off run of
  the same binary measured 710 / 4,108 / 3,190 / 4,022 / 684), so no
  further runs were made. Typing re-renders no pane with avatars on or off
  (asserted by the test).
- On, every one of the 50 members has an avatar, so each message and
  member row draws a slot and an image: channel switch and batch cost about
  10 % more than off, within the provisional targets' 20 %.
- Avatar bookkeeping at the end of the on run: 50 fetches for 50 distinct
  URLs (deduplicated across 10 channels, 200 switches and scrolling the
  whole log), 50 ready, 0 failed or evicted, 50 records, 409,600 charged
  bytes (50 × 32 × 32 × 4 × 2) of the 2 MiB budget, nothing in flight,
  nothing waiting for release.
- `model::Message` stays 64 bytes (`size_of`, printed by the test); avatar
  references live once per network in `app::avatars`.
- Metadata handling runs in each connection's existing worker loop. The
  only new timer is the deferred-sync sleep, armed only while a
  `RPL_METADATASYNCLATER` retry is pending (at most 16 channels, 3 tries
  each).

A GUI check with a `preview-fixture` release build under Xvfb (Mesa
llvmpipe) against a local IRC fixture (120 members, 40 with avatars, a
300 ms fixture delay per image) showed the avatar column and member icons
at 16×16 without any change of row spacing, blank slots for users without
avatars, the log staying put while scrolled up during a burst and
following again at the bottom, and the column disappearing at once when
the setting was turned off. The process-level scenarios (`run_baseline.py`)
were not run.

## Own avatar and later-joiner lookups (2026-09-28)

Focused request-count checks, not the baseline matrix (no measured UI hot
path changed: `ChatWindow::handle_events` only gains one scan of each
batch for own-avatar events, and the settings window is redrawn only for
batches that contain them). `model::Message` is unchanged: no avatar URL
or image byte is stored in messages, and our own avatar state is one small
struct per server session.

- Unit tests (`irc-core::metadata`): 200 JOINs in one burst keep 64
  lookups and one diagnostic; over 4 s at most 8 requests are in flight
  and at most one leaves per 500 ms; an unanswered lookup frees its slot
  after 30 s and is not retried; everything drains with at most 64
  requests in total.
- Against Ergo v2.19.1 on loopback (macOS, debug build,
  `metadata_interop::join_bursts_and_repeated_updates_stay_bounded`): 24
  users joining at once caused exactly 24 `GET` requests, the last
  answered 13.6 s after the burst (2 s pause + 23 × 0.5 s); 8 consecutive
  avatar changes by one member and a NAMES refresh caused no request.
  Consequence: in a large join burst the last joiners' avatars can take
  tens of seconds to appear (64 × 0.5 s ≈ 32 s), and joiners beyond 64
  pending get none until they change their avatar or rejoin.

## Message identity (2026-09-28)

A retained `model::Message` grew from 64 to 96 bytes (asserted by
`model::tests::retained_message_size_stays_bounded`): the source timestamp
(`Option<Timestamp>`, 16) and native identifier (`Option<Box<str>>`, 16; its
text, at most 128 bytes, is on the heap only when a server sent a msgid).
At 20,000 retained lines that is about 640 KiB more, a few percent of the
measured 250–550 bytes per line. The duplicate filter holds at most 512
64-bit keys per conversation (about 10 KiB with the hash set) and exists
only for conversations that received a msgid or server-time; with IRCv3 off
nothing is allocated. Hashing is bounded to 512 text bytes per line. No
timer, thread or render path changed, so the UI baseline was not rerun.

## Recent channel history (2026-09-28)

No measured hot path changed; bounds by construction. Per connection: at
most 64 channels queued, one request outstanding (so at most one reply
buffered: 100 lines), 8 abandoned channel names and 16 nested batch
references; the only timer is the outstanding request's 30 s timeout, armed
only while a request is outstanding. Per conversation: one pending
reservation (a `u64`), at most 256 inserted lines per reply, and the
existing 2,000-line bound applied after insertion. A reply is moved into
the log with one `Vec::splice`; nothing is cloned. With the option off no
state is allocated and the wire is unchanged.

## Older channel history pages (2026-09-28)

No measured hot path changed; bounds by construction, checked by tests
(`older_pages_stay_within_the_log_bound`,
`prepended_rows_of_any_height_do_not_move_the_viewport`). A page is asked
for only from a user's scroll event near the top of the main log; no timer
or polling exists, and an idle or merely open channel sends nothing. Per
conversation: one request on its way and a two-field state (created by the
first page, dropped with the session, the channel or the conversation). A
page adds at most 50 lines (100 kept if a server sends more) and never
takes the log past 2,000 lines, so repeated paging stops at the bound
instead of trimming what the user is reading. Insertion is one
`Vec::splice` at the front (moving at most 2,000 `Message` values, no
clones) and `LogList::sync` splices the new rows in one run; rows above
the viewport are not measured until drawn. Overlap checking builds a
temporary filter of at most 356 keys that is dropped afterwards, and the
conversation's 512-key filter is only read. Worker side: BEFORE shares the
64-entry queue and single outstanding request of recent history, and the
30 s timeout timer exists only while a request is outstanding.

## Reconnect gap recovery (2026-09-28)

No measured hot path changed; bounds by construction. At a disconnect the
application scans at most the last 256 lines of each joined channel once
and keeps one resume point per channel (a sequence, an optional msgid of
at most 128 bytes and a timestamp), reserving 256 sequences; points are
dropped once answered or given up, and with the conversation. The
reconnect configuration carries at most one entry per channel; the worker
keeps at most 1,024 and forgets each at its first join. Recovery requests
use the recent-history queue (one outstanding, 64 queued), so many
channels are paced one reply at a time rather than sent at once; each
adds at most 100 lines (one note line more) through the existing
insertion and the 2,000-line bound, and a long gap never triggers further
requests. No timer beyond the existing 30 s per-request timeout; nothing
remains after recovery.

## Presence index (2026-10-04, issue #71)

The worker's `PresenceIndex` (`irc-core/src/presence.rs`) is a second,
two-way copy of who is in which joined channel, next to `RosterTracker::last`.
It lives only in the worker, so the headless UI test (which bypasses the
worker) cannot see it; no UI, event or `model` type changed, so the process
scenarios and the UI test were not rerun for comparison. A single UI test run
(parallel with other tests, so noisy) stayed in the usual range. Instead the
ignored `presence::cost::presence_cost` test measures the index against
scanning the published rosters, on the Linux x86_64 container (12 cores,
rustc 1.99.0, release, three runs; times agreed within noise):

```sh
cargo test --release -p cayenchat-irc-core --lib presence_cost -- --ignored --nocapture
```

Heap bytes come from a counting allocator in that test. "Roster" is the
`HashMap<String, Vec<String>>` the tracker already keeps. Lookup is "is this
nickname in channel X" asked for every channel, as `had_member` does per QUIT
or NICK.

| Channels × members, distinct users | Roster | Index | Build (first NAMES / re-apply) | Lookup, all channels: scan → index | NICK / QUIT in the index |
| --- | --- | --- | --- | --- | --- |
| 10 × 50, 500 | 16 KiB | 162 KiB | 0.2–0.5 ms / 0.05–0.11 ms | 2–5 µs → 0.8–2 µs | 0.16 / 0.18 µs |
| 10 × 50, 100 | 16 KiB | 36 KiB | 0.05 ms / 0.05 ms | 1.9 µs → 0.8 µs | 0.12 / 0.18 µs |
| 20 × 5,000, 100,000 | 3.8 MiB | 13.7 MiB | 34 ms / 18–20 ms | 150 µs → 2 µs | 0.3 / 0.3 µs |
| 20 × 5,000, 20,000 | 3.5 MiB | 7.6 MiB | 16–18 ms / 14–15 ms | 210–230 µs → 3–4 µs | 0.4–0.6 / 0.5–0.7 µs |

- Cost: about 100–130 bytes per membership in the worst case, so the index
  adds roughly 2–3× the existing roster in large channels (a fourth copy,
  see "Resource bounds today"), and each NAMES snapshot costs a rebuild of
  that channel's entry (about 7 µs per member here). A channel switch or
  keystroke does not touch it. Channel keys are shared (`Arc<str>`) between
  the user and channel sides; the first version cloned a `String` per
  membership and used 17 MiB / 10 MiB in the large cases.
- Benefit: the QUIT/NICK affected-channel lookup no longer scans every
  roster (150–230 µs per event in a 100,000-membership connection, which
  matters in a netsplit), and NICK/QUIT in the index are sub-microsecond.
- Shared-channel judgments: accounts, metadata and peer avatars now ask the
  index instead of scanning the rosters. This costs one more `String` per
  user (the nickname as spelled, for `only_in`), which is the difference
  from the 11.7 / 6.7 MiB first measured for the two large rows. A
  `shares` check is the same kind of lookup as in the table (a hash lookup
  and a walk of the user's few channels, instead of a walk of every
  roster), and `PeerAvatars::speaker` calls it for each live message from
  an unknown user. The worker's end-to-end timing was not measured.
- Not measured: process footprint (`run_baseline.py`), real servers.
  Removing the duplicate roster copies is the follow-up listed under
  "Resource limit candidates"; the index is not bounded separately because
  it holds exactly the channels the library tracks as joined.

## Received line limit (2026-10-08, PR #238)

The vendored `irc-proto` codec now fails a line over 16 KiB (see "Resource
bounds today"); the only cost on the read path is one length comparison per
`decode` call. Compared against `38cc09e` (parent, master) and `83b2860`
(branch) in the Linux x86_64 container (12 vCPUs, rustc 1.99.0, Xvfb, not a
visible desktop window, so `window visible` is 0/1 and the CPU numbers are
only comparable between these builds). Both binaries are
`cargo build --release --locked -p cayenchat-ui`; the parent was built in a
separate worktree and target directory. Parent and branch alternate, three
runs each.

```sh
xvfb-run -a -s '-screen 0 1280x900x24' python3 scripts/perf/run_baseline.py --runs 1 --binary BIN --out OUT
cargo test --release --locked -p cayenchat-ui perf_baseline::perf_baseline -- --ignored --exact --nocapture
```

Process (`run_baseline.py`, one run per invocation; parent / branch, runs 1–3):

| Scenario | RSS MiB | CPU % |
| --- | --- | --- |
| S2 connected, idle | 113.8, 113.9, 113.9 / 114.0, 113.5, 113.6 | 0.00–0.03 both |
| S3 after 2,000 lines per channel | 118.4, 118.7, 118.5 / 118.6, 118.1, 118.1 | 0.00–0.03 both |
| S4a 200 lines/s | 118.4, 118.7, 118.5 / 118.6, 118.1, 118.1 | 3.82–4.38 / 3.65–4.41 |
| S4b overload | 119.5, 119.4, 119.7 / 119.7, 119.5, 119.4 | 190, 78, 190 / 84, 190, 190 |
| S5 after saturation | 119.5, 119.6, 119.7 / 119.7, 119.5, 119.4 | 0.00–0.03 both |
| S6 second overload, idle | 119.8, 120.3, 119.9 / 119.7, 119.8, 119.6 | 0.00–0.03 both |

- Memory and idle CPU agree within a few tenths of a MiB; S5 to S6 stays on a
  plateau in every run. Peak RSS is not recorded by the runner.
- S4b throughput: in four of the six runs (two parent, two branch) the
  fixture saw two connections and a 15 s median PING round trip with about
  4.9 million lines read at 162,000–164,500 lines/s; in the other two (one
  each) the fixture saw one connection and read 1.5–1.8 million lines at
  51,000–60,000 lines/s. This split appears in both builds, so it comes from
  the fixture or Xvfb run and not from the change; within each group the
  parent and branch rates are the same (for example 162,697 / 163,900 lines/s
  in the two-connection group).

Headless UI test (medians in µs; parent / branch, runs 1–3):

| Typing | Channel switch | Scroll 20 rows | 256-event batch | Typing after |
| --- | --- | --- | --- | --- |
| 248 / 247, 285 / 246, 244 / 294 | 1,653 / 1,685, 1,992 / 1,651, 1,658 / 1,649 | 1,414 / 1,223, 1,250 / 1,239, 1,240 / 1,257 | 2,244 / 2,101, 1,643 / 1,661, 1,676 / 1,691 | 260 / 329, 256 / 260, 259 / 258 |

- Equal within run-to-run noise (single outliers on either side, such as the
  parent's 285 µs typing in run 2 and the branch's 294 µs in run 3). The test
  bypasses the IRC worker, so it shows that nothing changed above the codec;
  the process runs above are what exercise the receive path.
- Not measured: a single line near the limit under load, and a real server.
  `irc-core`'s worker test checks that an endless line without a newline
  ends the connection with the reason.

## encoding_rs codec (2026-10-08, PR #252)

The vendored `irc-proto` codec decodes and encodes each line with
`encoding_rs` instead of the `encoding` crate. Compared against `c98d39d`
(master) and `afe7613` (branch) under the same conditions and commands as the
received line limit above. Builds alternate with #250 (`d6ecb1e`), three runs
each, and each run starts only after no other measurement or rustc process has
been seen for about a minute.

Process (RSS MiB at the end of each scenario / CPU %; master / branch, runs 1–3):

| Scenario | RSS MiB | CPU % |
| --- | --- | --- |
| S2 connected, idle | 108.9, 109.0, 108.6 / 109.0, 113.6, 109.0 | 0.00–0.03 both |
| S3 after 2,000 lines per channel | 122.7, 113.7, 113.3 / 113.8, 138.4, 114.0 | 0.00–0.03 both |
| S4a 200 lines/s | 138.3, 113.7, 113.3 / 113.8, 138.4, 114.0 | 3.96–4.17 / 2.90–3.96 |
| S4b overload | 138.6, 114.1, 113.6 / 114.1, 138.7, 115.0 | 196, 196, 196 / 194, 194, 188 |
| S6 second overload, idle | 138.7, 114.3, 113.8 / 114.2, 138.8, 139.4 | 0.00–0.03 both |

- RSS settles at either about 114 MiB or about 139 MiB in both builds (and in
  #250), so the step comes from the allocator or Xvfb run, not the codec.
- Binary size: 55,435,288 / 54,900,616 bytes (−535 KB without the `encoding`
  crate's tables).

Headless UI test (medians in µs; master / branch, runs 1–3):

| Typing | Channel switch | Scroll 20 rows | 256-event batch | Typing after |
| --- | --- | --- | --- | --- |
| 232 / 233, 227 / 269, 234 / 239 | 1,548 / 1,591, 1,560 / 1,890, 1,583 / 1,611 | 1,218 / 1,219, 1,137 / 1,400, 1,215 / 1,226 | 1,574 / 1,606, 1,585 / 2,175, 1,656 / 1,601 | 262 / 291, 253 / 340, 254 / 253 |

- Branch run 2 is slower on every step, including typing, which the test runs
  without the codec; runs 1 and 3 match master within noise. The test bypasses
  the IRC worker, so the process runs are what exercise the codec.

## Bidirectional control neutralization (2026-10-08, PR #250)

`model::display::neutralize_bidi` scans each new message's text (and topics,
server log lines, notifications and WHOIS rows) when it is stored, and names
(senders, channels, members, dialog titles) when they are drawn. It allocates
only when a control is present. The measurements below were taken at `d6ecb1e`,
which scanned the sender at storage time and not at draw time; a later
independent UI run at the current head showed no large change. Compared against `c98d39d` (master) and `d6ecb1e`
(branch) under the same conditions and commands as the received line limit
above. Builds alternate with #252 (`afe7613`), three runs each, and each run
starts only after no other measurement or rustc process has been seen for about
a minute.

Process (RSS MiB at the end of each scenario / CPU %; master / branch, runs 1–3):

| Scenario | RSS MiB | CPU % |
| --- | --- | --- |
| S2 connected, idle | 108.9, 109.0, 108.6 / 109.4, 108.9, 108.6 | 0.00–0.03 both |
| S3 after 2,000 lines per channel | 122.7, 113.7, 113.3 / 138.4, 113.7, 113.4 | 0.00–0.03 both |
| S4a 200 lines/s | 138.3, 113.7, 113.3 / 138.5, 113.7, 113.4 | 3.96–4.17 / 2.89–4.13 |
| S4b overload | 138.6, 114.1, 113.6 / 138.8, 114.2, 113.7 | 196, 196, 196 / 196, 195, 194 |
| S6 second overload, idle | 138.7, 114.3, 113.8 / 138.9, 114.3, 113.8 | 0.00–0.03 both |

- RSS settles at either about 114 MiB or about 139 MiB in both builds (and in
  #252), so the step comes from the allocator or Xvfb run, not the change.
- Binary size: 55,435,288 / 55,442,672 bytes.

Headless UI test (medians in µs; master / branch, runs 1–3):

| Typing | Channel switch | Scroll 20 rows | 256-event batch | Typing after |
| --- | --- | --- | --- | --- |
| 232 / 242, 227 / 232, 234 / 1,074 | 1,548 / 1,585, 1,560 / 1,529, 1,583 / 4,678 | 1,218 / 1,200, 1,137 / 1,165, 1,215 / 3,690 | 1,574 / 1,627, 1,585 / 1,528, 1,656 / 4,866 | 262 / 254, 253 / 243, 254 / 676 |

- Branch run 3 is about three times slower on every step, typing included,
  which the change does not touch: outside load. Runs 1 and 2 match master
  within noise, including the 256-event batch that goes through `new_message`.

## Mentions and keywords once per message (2026-10-09, PR #255)

`ChatWindow::find_mentions` now runs on the UI thread once per live message
from someone else (formatting-free text, two `plain_ranges` passes) and
stores the ranges in `Message::highlights`; the log no longer searches text
while drawing. `Message` grows from 104 to 112 bytes (`message_size` in the
UI test output), about 160 KiB at 20,000 retained lines. Compared against
`45d6f07` (parent, master) and `690dd69` (branch, clean) in the Linux x86_64
container (12 vCPUs, rustc 1.99.0, Xvfb, not a visible desktop window, so the
CPU numbers are only comparable between these builds). Both binaries are
`cargo build --release --locked -p cayenchat-ui`; the parent was built in a
separate worktree and target directory. Parent and branch alternate, three
runs each (process runs first, then the UI test), with the same commands as
the received line limit above.

Process (`run_baseline.py`, one run per invocation; parent / branch, runs 1–3):

| Scenario | RSS MiB | CPU % |
| --- | --- | --- |
| S2 connected, idle | 115.7, 115.8, 115.9 / 116.1, 116.2, 115.4 | 0.00 both |
| S3 after 2,000 lines per channel | 120.4, 120.4, 120.7 / 120.4, 120.5, 119.8 | 0.00–0.03 both |
| S4a 200 lines/s | 120.4, 120.4, 120.7 / 120.5, 120.5, 119.8 | 4.00, 4.34, 3.96 / 4.03, 4.27, 4.03 |
| S4b overload | 120.8, 120.8, 121.0 / 121.9, 121.6, 121.0 | 189.6, 189.2, 189.8 / 184.0, 183.8, 184.3 |
| S6 second overload, idle | 120.8, 121.0, 121.2 / 122.1, 121.8, 121.2 | 0.00–0.03 both |

- S4a (200 lines/s) PING round trip max: 1.3, 1.4, 1.2 ms / 1.1, 1.0, 0.8 ms.
  CPU and latency at a realistic rate are the same.
- S4b throughput (lines/s the client read in 30 s): 171,549, 174,956, 180,083
  / 165,881, 169,114, 172,928, so the branch reads about 3 % fewer lines
  (medians 174,956 and 169,114) while using about 3 % less CPU. In every run
  of both builds the fixture saw two connections and a 15 s median PING round
  trip, so the runs are comparable with each other. Per-line work added on the
  UI thread (the search; the flood's text rarely contains the nickname) is
  the likely cause, but it was not profiled. The 3 % applies only to a client
  that is already behind by seconds.
- RSS differs by at most 1.3 MiB, within the spread between runs of one build
  in earlier sections; the branch is slightly above the parent after S4b/S6
  in all three runs, consistent with the 8 extra bytes per retained message
  plus allocator noise, but the size of the effect is not separable here.
- Binary size: 54,915,752 (parent) / 54,940,376 (branch) bytes.

Headless UI test (medians in µs; parent / branch, runs 1–3):

| Typing | Channel switch | Scroll 20 rows | 256-event batch | Typing after |
| --- | --- | --- | --- | --- |
| 241 / 240, 240 / 242, 239 / 283 | 1,567 / 1,574, 1,604 / 1,535, 1,587 / 1,683 | 1,177 / 1,192, 1,186 / 1,229, 1,206 / 1,240 | 1,564 / 1,589, 1,590 / 1,621, 1,562 / 1,604 | 251 / 252, 252 / 249, 248 / 265 |

- Branch run 3 is slower on typing and channel switching, which the change
  does not touch; its other runs match the parent. The 256-event batch is
  1–2 % slower on the branch in each run, which is the UI-thread search added
  to `new_message`'s path. Scrolling is 1–3 % slower, not faster, although the
  log no longer searches while drawing: the test's lines contain no
  highlights, so the draw path only loses a cheap scan, while `Message` is
  larger. The differences are at the edge of the run-to-run noise.
- Limits: one container, Xvfb, not a visible window. The test bypasses the IRC
  worker (so the removal of the nickname check there is not seen) and its
  lines carry no mentions or formatting codes, so the cost of lines that do
  have highlights (the `Highlights` allocation) is not measured. Real servers
  were not used.

## Resource limit candidates (proposal)

These are not agreed. Each needs a decision before it is implemented. The
image fetch and decoded-cache rows were decided for inline previews (D018 and
the image preview section above); icons are still open.

| Candidate | Proposal | Basis |
| --- | --- | --- |
| Application-wide retained-message budget | Keep 2,000 per conversation, and add a total across all networks (for example 100,000 lines, about 25–55 MiB at the measured 250–550 bytes per line), evicting from conversations viewed least recently | Today only per-conversation and per-network bounds exist: 1,000 conversations × 2,000 lines is allowed per network, and multi-server multiplies it. |
| Conversations | Keep 1,000 per network; add an application-wide ceiling | Same; each conversation also owns a `TextInput` entity and a `LogList`. |
| Diagnostics | 1,000 lines per server plus an application-wide ceiling | IRC lines are formatted only while the transcript can be shown (registration, or the debug transcript on), so the remaining cost is the retained lines. |
| Event handling fairness | Keep 512 events per connection; drain connections round-robin so one flooded server cannot delay another | Measured: a single overloaded connection builds about 1–2 s of worker-side lag. |
| Redraw rate under traffic | Coalesce redraws caused by incoming lines (for example at most 30 per second while not interacting) | 200 lines/s costs 16 % of a core when drawn versus 2–3 % when hidden. Needs a decision because it trades latency for CPU. |
| Rosters | Keep one copy per channel, or bound the extra copies | Rosters are stored three times today; large channels multiply this with every added server. |
| Image fetch | HTTP(S) only; at most a few concurrent fetches app-wide (for example 4) and per server (for example 2); a bounded queue that drops requests for rows scrolled out of view; body size and redirect limits | Fetching must never grow without bound during floods of links. |
| Decoded image cache | Budget in decoded bytes (width × height × 4), not in entries (for example 64 MiB), with a per-image pixel limit (for example 4096 × 4096); GPU texture bytes counted against the same or a separate budget | Compressed size says little about memory; the settings window alone costs about 28 MiB of footprint, so GPU-side memory is significant. |
| Icons | A separate, small decoded-byte budget | Icons are many and small; they must not evict previews or the reverse. User avatars now have one (2 MiB, D023); channel/network icons are still open. |

When a feature is off (icons, previews, experimental IRCv3 extensions) its
caches, queues and threads must not exist, and the S1–S6 numbers must match
the baseline.

## Provisional targets (proposal)

Derived from the baseline above; not agreed. Re-evaluate once visible load
runs and a second platform are measured.

- Idle (S2, S3, S5): CPU median at most 0.5 % of a core and at most one idle
  wakeup per second, independent of the number of connections.
- Memory: footprint after S6 within 10 % of S5 (the baseline ranged from
  −5 % to +8 %, no sustained growth after saturation);
  per-retained-line cost not above about 600 bytes.
- Headless UI test at 10 channels: typing median at most 0.5 ms with no pane
  re-render; channel switch and 256-event batch medians at most 2.5 ms. A
  change that raises a median by more than 20 % needs an explanation.
- Steady 200 lines/s with the window visible: at most 20 % of a core, PING
  round trip p95 below 5 ms.

## Improvement candidates (not part of the baseline work)

Observed while measuring; each is a separate task if pursued:

- Coalesce redraws triggered by incoming traffic (see the redraw candidate
  above).
- Format wire diagnostics lazily.
- Make the combined-subwindow rebuild incremental (the heap merge already
  bounds it by the rows shown; an incremental update would avoid it too).
- Send roster deltas instead of full NAMES snapshots on JOIN/PART in large
  channels, and remove duplicate roster copies.
- Drop per-conversation UI state (`main_lists`, draft inputs without text)
  for conversations that were parted and are no longer shown.

## Shortened URL bubble (2026-10-07)

A short check, not a baseline run, in the Linux container used for this
change (x86_64, shared vCPUs, rustc 1.99.0). Headless UI test in release
mode, 10 channels × 2,000 lines, three alternating runs each, medians in µs
(parent `64f165a` / branch):

| Run | Typing | Channel switch | Scroll 20 rows | 256-event batch | Typing after |
| --- | --- | --- | --- | --- | --- |
| 1 | 235 / 244 | 1,485 / 1,601 | 1,169 / 1,203 | 1,589 / 1,630 | 240 / 254 |
| 2 | 245 / 242 | 1,618 / 1,619 | 1,193 / 1,216 | 1,597 / 1,598 | 258 / 251 |
| 3 | 246 / 243 | 1,586 / 1,608 | 1,209 / 1,207 | 1,595 / 1,719 | 255 / 254 |

- The two builds agree within run-to-run noise. The bubble's logic runs
  only while a bubble is shown or being scheduled (one mouse-move check per
  frame in the tooltip layer); nothing is added to the per-message or
  per-frame paths otherwise. The test does not turn shortened URLs on or
  hover a URL; that path is covered by `scripts/e2e/url_bubble_gui.py`
  (behaviour) and `scripts/perf/url_bubble_perf.py` (see below).

With shortened URLs on (`perf_baseline_short_urls`: every second line has two
long URLs, `compact_urls` enabled; same machine and build, parent `64f165a`
with the same test file / branch, three alternating runs, medians in µs):

| Run | Typing | Channel switch | Scroll 20 rows | 256-event batch |
| --- | --- | --- | --- | --- |
| 1 | 262 / 267 | 1,771 / 1,807 | 1,400 / 1,421 | 1,906 / 3,016 |
| 2 | 264 / 262 | 1,783 / 1,753 | 1,396 / 1,384 | 1,840 / 1,853 |
| 3 | 264 / 270 | 1,750 / 1,761 | 1,375 / 1,400 | 1,868 / 1,862 |

- Equal within noise; the one 3,016 µs batch is a single outlier (the same
  run's other medians match, and runs 2 and 3 give 1,853 / 1,862). The
  headless test cannot hover, so showing, replacing and hiding a bubble is
  measured under Xvfb below (`scripts/perf/url_bubble_perf.py`); that path
  runs only while a bubble is shown or scheduled.

#### Bubble path under Xvfb (CPU and memory, issue #232)

`scripts/perf/url_bubble_perf.py` drives the hover path of a release binary
under Xvfb with shortened URLs on (same loopback server and layout as
`scripts/e2e/url_bubble_gui.py`) and samples the app process with
`sample_process.py`: 15 s idle, three rounds of 10 pointer cycles (1.5 s on
each stop; off the log, first URL, second URL, off the log), 15 s idle.
CPU is the app process only. Builds: parent `64f165ac41532793fd8f0b10fdb41d9deeed4f4b`
(separate worktree and target directory) and this branch's code at
`becc1b326ec2bb66c13266b4db24d61d550793d7` (plus the script), both
`cargo build --release --locked -p cayenchat-ui`, rustc 1.99.0, x86_64 Linux,
12 vCPUs, Mesa software rendering. Parent and branch alternate. The parent
does not replace a bubble when the pointer moves to another URL, so the
second stop only does work on the branch. `--first-only` leaves the second
stop out (off, first URL, off), which both builds treat alike.

CPU % of one core per round (three rounds each, three alternating runs; two
runs for `--first-only`):

| Scenario | Parent | Branch |
| --- | --- | --- |
| Idle before / after | 0.86–0.93 | 0.86–0.93 |
| Pointer resting on a shown bubble (20 s) | 0.89 | 0.89 |
| 4-stop cycle (first, second) | 4.34–4.79 | 6.46–6.80 |
| `--first-only` cycle | 4.41–4.90 | 5.76–6.28 |

RSS at the end of each round: constant across the three rounds in every run
(parent 141.9–143.6 MB, branch 142.3–144.1 MB; the run-to-run spread is as
large as the difference), and idle CPU after the cycles equals idle CPU
before. Thread count stayed at 47.

- Nothing grows with repetition, and nothing runs while a bubble merely
  stays up. Memory is unchanged.
- The branch costs more CPU per bubble that comes and goes. A one-off check
  (five times: pointer onto the URL, 8 s wait, off the log, 4 s wait) gave
  the same CPU for showing (about 180 ms on both, including idle) and about
  65 ms more for hiding on the branch (about 80 ms vs 150 ms, including idle).
  The bubble is now hoverable, so it stays for the hide delay and the
  pointer check runs on its frames. The cost is per hover, not per message
  or per frame, and the 1–2 point difference above comes from the test
  moving the pointer every 1.5 s. The extra 0.5–0.7 point in the 4-stop cycle
  is the second bubble that the branch now draws.
- Those one-off figures came from throwaway scripts that are not committed;
  the committed script gives the cycle figures.

## Channel case mapping and index (2026-10-08, PR #247)

Channel names are compared with the network's advertised `CASEMAPPING`, and
`AppState::channel_id` is one hash lookup instead of a scan of every
conversation (one folded `String` per lookup); the worker copies the joined
list only when a check is made. Compared against `5587533` (parent, master)
and `fcd0eac` (branch) in the Linux x86_64 container (12 vCPUs, rustc 1.99.0,
Xvfb, no visible window, so `window visible` is 0/1 and CPU numbers are only
comparable between these builds). Release builds in separate worktrees,
parent and branch alternating, three runs each, nothing else running
(`pgrep` checked before).

```sh
xvfb-run -a -s '-screen 0 1280x900x24' python3 scripts/perf/run_baseline.py --runs 1 --binary BIN --out OUT
cargo test --release --locked -p cayenchat-ui perf_baseline::perf_baseline -- --ignored --exact --nocapture
```

Process (parent / branch, runs 1–3):

| Scenario | RSS MiB | CPU % |
| --- | --- | --- |
| S2 connected, idle | 108.2, 108.4, 108.4 / 108.3, 108.1, 108.1 | 0.00–0.03 both |
| S3 after 2,000 lines per channel | 113.1, 113.2, 113.1 / 113.0, 113.0, 112.7 | 0.00–0.03 both |
| S4a 200 lines/s | 113.1, 113.2, 113.2 / 113.0, 113.1, 112.7 | 4.03, 4.03, 3.96 / 3.93, 3.89, 4.20 |
| S4b overload | 113.4, 113.4, 113.5 / 113.4, 113.4, 113.0 | 195, 196, 197 / 191, 190, 190 |
| S5 after saturation | 113.4, 113.4, 113.5 / 113.4, 113.4, 113.0 | 0.00–0.03 both |
| S6 second overload, idle | 113.6, 113.6, 113.6 / 113.5, 113.7, 113.3 | 0.00–0.03 both |

- Memory and idle CPU agree within a few tenths of a MiB; S5 to S6 stays on a
  plateau. S4a CPU is the same within noise.
- S4b throughput (all six runs saw two connections): parent 160,267 /
  159,599 / 162,625 lines/s, branch 156,848 / 157,839 / 158,262 lines/s, i.e.
  about 2–3 % fewer lines read per second on the branch, with CPU also about
  3 % lower. The fixture, not the client, paces this overload, so it is not
  evidence of a slower receive path; but the spread between the groups does
  not overlap, so a small cost on the saturated path cannot be ruled out. The
  fixture's lines go to joined channels, so the folded key allocation per
  lookup is exercised here.

Headless UI test (medians in µs; parent / branch, runs 1–3):

| Typing | Channel switch | Scroll 20 rows | 256-event batch | Typing after |
| --- | --- | --- | --- | --- |
| 241 / 244, 238 / 224, 232 / 240 | 1,612 / 1,592, 1,571 / 1,468, 1,505 / 1,588 | 1,192 / 1,209, 1,153 / 1,142, 1,179 / 1,195 | 1,589 / 1,606, 1,600 / 1,566, 1,510 / 1,586 | 251 / 254, 257 / 314, 254 / 252 |

- Equal within run-to-run noise (the branch's 314 µs typing-after in run 2 is
  a single outlier). The 256-event batch goes through `channel_id`.
- Not measured: a real server, and a network with many hundreds of channels
  (where the index replaces a linear scan).

## PING timeout under overload (2026-10-09, issue #272)

The disconnect seen in S4b was the `irc` crate's PING timeout. The crate sends
its own PING 180 s after connecting and fails the stream with `PingTimeout`
when no PONG came within `ping_timeout` (default 20 s). It checks that
deadline before it looks at buffered data, and the worker reads the socket
only while the event queue has room, so under a flood the PONG waits in the
socket buffer (fixture PING round trip 15 s median, 29 s max). In a full
`run_baseline.py` run the 180 s point falls into S6, the second maximum flood;
the fixture logged the disconnect at 200.4 s (180 s + 20 s). `--load-only`
ends S4b before 180 s and saw one connection. The fixture's own PINGs are
answered by the client and are not involved.

`irc-core::PING_TIMEOUT_SECS` now sets `ping_timeout` to 120 s. The backlog
is bounded by the socket buffer, so a dead connection is still detected, 120 s
after the PING. A bouncer replaying a large history at connect is not
affected by this timeout (the first PING is at 180 s), but any long
backlog at that moment would have hit it in the same way.

Verification (Linux, Xvfb, release build): before, the one full run made after
reproducing showed `connections 2` with the disconnect at 200.4 s; after, three
full runs showed `connections 1` each (S4b CPU 183–184 %, RSS 120 MiB). S4b
throughput and CPU were not otherwise compared, since the change touches no
per-line path.
