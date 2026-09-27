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
| Diagnostics transcript: newest 1,000 lines per server in a `VecDeque` | `ui::session` `DIAGNOSTIC_LIMIT` | Every IRC line is recorded, so it must be bounded and O(1) to trim. |
| Combined subwindow: newest 1,000 lines across other channels, rebuilt only when the last message sequence or the selection changes, by merging conversation tails newest-first | `ui` `SUB_LOG_LIMIT`, `sync_log_lists`, `newest_lines` | Keystrokes do not rebuild it, and the rebuild does not grow with every retained line when there are many conversations. |
| At most 1,000 conversations per network; messages for unknown channels go to the bounded server log | `app` `MAX_CONVERSATIONS_PER_NETWORK` | A hostile server or bouncer cannot grow memory without limit. |
| WHOIS collection capped (32 pending nicknames, 512 items each); rosters cached only for joined channels | `irc-core` `MAX_PENDING_WHOIS`, `MAX_WHOIS_ITEMS` | Same. |
| Virtualized panes: both logs, user list and channel tree use GPUI `list`/`uniform_list` with 400 px overdraw | `ui::log_list` | Only rows near the viewport are laid out. |
| `LogList::sync` splices only rows whose sequences appeared or disappeared | `ui::log_list` | Measured row heights and scroll position survive appends, trims and channel switches. |
| The four panes are separate views drawn with `AnyView::cached` | `ui` `ChatPane`, `ChatPanes` | A keystroke redraws only the window shell and the draft; a GPUI test (`typing_reuses_panes_and_new_messages_redraw_them`) asserts it. State changes that affect panes must `cx.notify()` the chat window. |
| Message times stored as minutes (`TimeOfDay`); a retained `Message` is 64 bytes plus its sender and text | `model` | Smaller logs, one fewer allocation per line. |
| QUIT/NICK republish rosters only for channels that contained the user; roster sort keys computed once per member | `irc-core` `RosterTracker`, `app` `sorted_members` | Large channels. |
| Linux: bounded per-word shaping cache in the vendored GPUI (swept every 128 lines, entries unused for four sweeps dropped) | `vendor/gpui`, see `PATCHES.md` | A channel switch does not reshape every newly visible line from scratch (issue #5). |

## Resource bounds today

| Resource | Bound | Scope | Notes |
| --- | --- | --- | --- |
| Channel log | 2,000 messages, trimmed to 1,000 | per conversation | No application-wide bound: 1,000 conversations × 2,000 lines is allowed. |
| Server log | 2,000 messages, trimmed to 1,000 | per network | |
| Conversations | 1,000 | per network | |
| Diagnostics transcript | 1,000 lines | per server | Formatted eagerly for every IRC line, shown or not. No application-wide bound. |
| Combined subwindow | 1,000 rows (indices only) | window | Rebuilt by a heap merge of conversation tails that stops at 1,000 rows. |
| Worker→UI events | 512 | per connection | Back-pressure, not a drop. |
| UI→worker commands | 128 | per connection | `try_send`; a full queue rejects the command. |
| WHOIS collection | 32 nicknames × 512 items | per connection | |
| Rosters | none | per channel | Kept three times: `irc`'s channel lists, `irc-core`'s `RosterTracker`, and `app`'s sorted `members`. |
| Per-selection UI state | none | per visited server/channel | `main_lists` (one `LogList` with measured heights) and one `TextInput` entity per conversation; cleared only when a connection is applied from settings (automatic reconnects keep them). |
| Attachment | 32 MiB | one upload at a time | Upload only; nothing is displayed inline. |
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
  private-message traffic (currently routed to the server log).
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

## Comparing image display

When image previews or icons are added, compare against this baseline with
both settings off (must match the baseline: no fetch, no decode, no extra
threads or wakeups) and on:

- Footprint after S3 with one image link per 20 lines, and after S5.
- Decoded-cache bytes and GPU texture bytes against their configured caps,
  after scrolling the whole retained log.
- Typing and channel switching in the UI test with previews on: row heights of
  previews must not force relayout of every line.
- Main-thread CPU during S4a with image links: fetch and decode must run off
  the UI thread; `sample` should show no decoding on the main thread.
- Number of network requests for repeated URLs (cache hits) and for links
  scrolled out of view.

## Resource limit candidates (proposal)

These are not agreed. Each needs a decision before it is implemented.

| Candidate | Proposal | Basis |
| --- | --- | --- |
| Application-wide retained-message budget | Keep 2,000 per conversation, and add a total across all networks (for example 100,000 lines, about 25–55 MiB at the measured 250–550 bytes per line), evicting from conversations viewed least recently | Today only per-conversation and per-network bounds exist: 1,000 conversations × 2,000 lines is allowed per network, and multi-server multiplies it. |
| Conversations | Keep 1,000 per network; add an application-wide ceiling | Same; each conversation also owns a `TextInput` entity and a `LogList`. |
| Diagnostics | 1,000 lines per server plus an application-wide ceiling; store the wire line and format on display | Every IRC line is formatted into a `String` whether the transcript is shown or not; with several servers this repeats per server. |
| Event handling fairness | Keep 512 events per connection; drain connections round-robin so one flooded server cannot delay another | Measured: a single overloaded connection builds about 1–2 s of worker-side lag. |
| Redraw rate under traffic | Coalesce redraws caused by incoming lines (for example at most 30 per second while not interacting) | 200 lines/s costs 16 % of a core when drawn versus 2–3 % when hidden. Needs a decision because it trades latency for CPU. |
| Rosters | Keep one copy per channel, or bound the extra copies | Rosters are stored three times today; large channels multiply this with every added server. |
| Image fetch | HTTP(S) only; at most a few concurrent fetches app-wide (for example 4) and per server (for example 2); a bounded queue that drops requests for rows scrolled out of view; body size and redirect limits | Fetching must never grow without bound during floods of links. |
| Decoded image cache | Budget in decoded bytes (width × height × 4), not in entries (for example 64 MiB), with a per-image pixel limit (for example 4096 × 4096); GPU texture bytes counted against the same or a separate budget | Compressed size says little about memory; the settings window alone costs about 28 MiB of footprint, so GPU-side memory is significant. |
| Icons | A separate, small decoded-byte budget | Icons are many and small; they must not evict previews or the reverse. |

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
