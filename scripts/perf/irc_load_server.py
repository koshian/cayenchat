#!/usr/bin/env python3
"""Minimal local IRC server that generates load for performance baselines.

It accepts one plaintext client on 127.0.0.1, completes registration, answers
the client's JOINs with a NAMES roster, and then runs the phases requested on
the command line or over its control socket. It never talks to a public
network and never handles credentials.

Phases (control socket commands, one per line):

  history N            send N PRIVMSG lines to every joined channel at full speed
  flood RATE SECONDS   send PRIVMSG lines round-robin across channels;
                       RATE 0 means as fast as the client consumes them
  probe SECONDS        send only a PING every second and record the round
                       trip (how quickly this server's lines are read while
                       another server is busy)
  joined               reply once the client has joined --expect-channels channels
  wait                 reply once the previous phase has finished
  stats                reply with the JSON summary so far (includes the number
                       of client connections; more than one means it reconnected)
  quit                 close the client and exit

Every phase ends with a PING marker. The time until the client's PONG is the
"drain lag": how long the client took to read everything queued before it.
The client's IRC worker only reads the socket while its bounded event queue
has room, so this approximates how far event handling fell behind. It is not
the time until lines appear on screen.

During a flood the server also sends a PING every second and records the
PONG round trip for the same reason.

With --image-every N, every Nth message carries one of --image-links links to
https://images.cayenchat.test/imgNNN.png. Only a client built with the
`preview-fixture` feature resolves those (from a local directory); nothing
is fetched from the network.
"""

import argparse
import asyncio
import json
import random
import sys
import time

SAMPLE_TEXTS = [
    "short line",
    "a somewhat longer line of ordinary IRC chatter about the build and the release plan",
    "日本語のメッセージです。チャンネル切替と入力の応答性を確認するための行です。",
    "mixed 日本語 and English text with a URL https://example.invalid/path?q=1",
    "とても長い行: " + "あいうえおかきくけこ" * 8,
    "0123456789 " * 12,
]


def rtt_summary(rtts, sent):
    rtts = sorted(rtts)
    if not rtts:
        return {"count": 0, "of": sent}
    return {
        "count": len(rtts),
        "of": sent,
        "median": round(rtts[len(rtts) // 2] * 1000, 1),
        "p95": round(rtts[min(len(rtts) - 1, int(len(rtts) * 0.95))] * 1000, 1),
        "max": round(rtts[-1] * 1000, 1),
    }


class Stats:
    def __init__(self):
        self.started = time.monotonic()
        self.phases = []
        self.lines_sent = 0
        self.connections = 0
        self.bytes_sent = 0
        self.client_lines = []

    def elapsed(self):
        return round(time.monotonic() - self.started, 3)

    def summary(self):
        return {
            "connections": self.connections,
            "lines_sent": self.lines_sent,
            "bytes_sent": self.bytes_sent,
            "phases": self.phases,
            "client_privmsg": len(self.client_lines),
        }


class Server:
    def __init__(self, args):
        self.args = args
        self.stats = Stats()
        self.writer = None
        self.nick = None
        self.channels = []
        self.joined = asyncio.Event()
        self.registered = asyncio.Event()
        self.pongs = {}
        self.ping_seq = 0
        self.busy = asyncio.Lock()
        self.rng = random.Random(args.seed)
        self.members = [f"user{i:03d}" for i in range(args.members)]
        self.phase_done = asyncio.Event()
        self.phase_done.set()
        self.sent_messages = 0

    # --- wire -----------------------------------------------------------------

    async def send(self, line):
        data = (line + "\r\n").encode("utf-8")
        self.writer.write(data)
        self.stats.bytes_sent += len(data)
        await self.writer.drain()

    async def ping(self):
        self.ping_seq += 1
        token = f"perf-{self.ping_seq}"
        future = asyncio.get_running_loop().create_future()
        self.pongs[token] = future
        sent = time.monotonic()
        await self.send(f"PING :{token}")
        return token, sent, future

    def message(self, channel):
        sender = self.rng.choice(self.members)
        text = self.rng.choice(SAMPLE_TEXTS)
        self.sent_messages += 1
        every = self.args.image_every
        if every and self.sent_messages % every == 0:
            link = self.sent_messages // every % self.args.image_links
            text = f"see https://images.cayenchat.test/img{link:03d}.png"
        return f":{sender}!{sender}@load.invalid PRIVMSG {channel} :{text}"

    # --- client handling ------------------------------------------------------

    async def handle_client(self, reader, writer):
        if self.writer is not None:
            writer.close()
            return
        self.writer = writer
        self.stats.connections += 1
        if self.stats.connections > 1:
            # A reconnect invalidates the measurement; report it and serve
            # the new session from scratch.
            print(f"[{self.stats.elapsed()}] client reconnected", file=sys.stderr)
            self.channels = []
            self.joined.clear()
        try:
            while True:
                raw = await reader.readline()
                if not raw:
                    break
                line = raw.decode("utf-8", "replace").rstrip("\r\n")
                await self.handle_line(line)
        except (ConnectionError, asyncio.IncompleteReadError):
            pass
        print(f"[{self.stats.elapsed()}] client disconnected", file=sys.stderr)
        self.writer = None

    async def handle_line(self, line):
        if line.startswith(":"):
            line = line.split(" ", 1)[1] if " " in line else ""
        command, _, rest = line.partition(" ")
        command = command.upper()
        if command == "NICK":
            self.nick = rest.lstrip(":").strip()
        elif command == "USER" and self.nick:
            await self.welcome()
        elif command == "PING":
            await self.send(f":load.invalid PONG load.invalid :{rest.lstrip(':')}")
        elif command == "PONG":
            token = rest.split(":", 1)[-1].strip()
            future = self.pongs.pop(token, None)
            if future and not future.done():
                future.set_result(time.monotonic())
        elif command == "JOIN":
            for channel in rest.split(" ")[0].lstrip(":").split(","):
                await self.join(channel)
        elif command == "PRIVMSG":
            self.stats.client_lines.append((self.stats.elapsed(), rest))
        elif command == "QUIT":
            pass

    async def welcome(self):
        nick = self.nick
        for line in [
            f":load.invalid 001 {nick} :Welcome to the CayenChat load fixture",
            f":load.invalid 002 {nick} :Your host is load.invalid",
            f":load.invalid 003 {nick} :This server was created for testing",
            f":load.invalid 004 {nick} load.invalid fixture-1 io ov",
            f":load.invalid 005 {nick} CHANTYPES=#& PREFIX=(ov)@+ NETWORK=Load :are supported",
            f":load.invalid 375 {nick} :- load.invalid Message of the day -",
            f":load.invalid 372 {nick} :- local performance fixture",
            f":load.invalid 376 {nick} :End of MOTD",
        ]:
            await self.send(line)
        self.registered.set()
        print(f"[{self.stats.elapsed()}] registered {nick}", file=sys.stderr)

    async def join(self, channel):
        if not channel or channel in self.channels:
            return
        nick = self.nick
        self.channels.append(channel)
        await self.send(f":{nick}!{nick}@client.invalid JOIN {channel}")
        names = [f"@{nick}"] + [
            ("+" if i % 10 == 0 else "") + member for i, member in enumerate(self.members)
        ]
        for start in range(0, len(names), 40):
            chunk = " ".join(names[start : start + 40])
            await self.send(f":load.invalid 353 {nick} = {channel} :{chunk}")
        await self.send(f":load.invalid 366 {nick} {channel} :End of NAMES list")
        if len(self.channels) >= self.args.expect_channels:
            self.joined.set()

    # --- phases ---------------------------------------------------------------

    async def marker(self, record):
        token, sent, future = await self.ping()
        try:
            received = await asyncio.wait_for(future, timeout=self.args.marker_timeout)
            record["drain_lag_s"] = round(received - sent, 3)
        except asyncio.TimeoutError:
            record["drain_lag_s"] = None
            print(f"marker {token} timed out", file=sys.stderr)

    async def history(self, per_channel):
        record = {"phase": "history", "per_channel": per_channel, "start": self.stats.elapsed()}
        started = time.monotonic()
        sent = 0
        for _ in range(per_channel):
            for channel in self.channels:
                await self.send(self.message(channel))
                sent += 1
        record["lines"] = sent
        record["write_s"] = round(time.monotonic() - started, 3)
        await self.marker(record)
        record["end"] = self.stats.elapsed()
        self.stats.lines_sent += sent
        return record

    async def flood(self, rate, seconds):
        record = {
            "phase": "flood",
            "target_rate": rate,
            "seconds": seconds,
            "start": self.stats.elapsed(),
        }
        rtts = []
        pending = []
        started = time.monotonic()
        next_ping = started + 1.0
        sent = 0
        index = 0
        channels = self.channels
        # Pace in 10 ms slices so a fixed rate is smooth rather than bursty.
        while True:
            now = time.monotonic()
            if now - started >= seconds:
                break
            if now >= next_ping:
                pending.append(await self.ping())
                next_ping += 1.0
            if rate > 0:
                due = int((now - started) * rate) + 1
                if sent >= due:
                    await asyncio.sleep(min(0.01, max(0.0, (sent + 1) / rate - (now - started))))
                    continue
                burst = min(due - sent, max(1, rate // 50))
            else:
                burst = 64
            for _ in range(burst):
                await self.send(self.message(channels[index % len(channels)]))
                index += 1
                sent += 1
        elapsed = time.monotonic() - started
        record["lines"] = sent
        record["write_s"] = round(elapsed, 3)
        record["achieved_rate"] = round(sent / elapsed, 1)
        await self.marker(record)
        for _, ping_sent, future in pending:
            if future.done():
                rtts.append(future.result() - ping_sent)
        if rtts:
            record["ping_rtt_ms"] = rtt_summary(rtts, len(pending))
        record["end"] = self.stats.elapsed()
        self.stats.lines_sent += sent
        return record

    async def probe(self, seconds):
        record = {"phase": "probe", "seconds": seconds, "start": self.stats.elapsed()}
        pending = []
        started = time.monotonic()
        while time.monotonic() - started < seconds:
            pending.append(await self.ping())
            await asyncio.sleep(1.0)
        rtts = []
        for _, ping_sent, future in pending:
            try:
                received = await asyncio.wait_for(future, timeout=self.args.marker_timeout)
                rtts.append(received - ping_sent)
            except asyncio.TimeoutError:
                pass
        record["ping_rtt_ms"] = rtt_summary(rtts, len(pending))
        record["end"] = self.stats.elapsed()
        return record

    async def run_phase(self, name, coro):
        async with self.busy:
            self.phase_done.clear()
            try:
                record = await coro
                self.stats.phases.append(record)
                print(json.dumps(record, ensure_ascii=False), file=sys.stderr)
            finally:
                self.phase_done.set()

    # --- control --------------------------------------------------------------

    async def control(self, reader, writer):
        try:
            while True:
                raw = await reader.readline()
                if not raw:
                    break
                words = raw.decode().split()
                if not words:
                    continue
                reply = "ok"
                if words[0] == "history":
                    await self.joined.wait()
                    asyncio.create_task(self.run_phase("history", self.history(int(words[1]))))
                elif words[0] == "flood":
                    await self.joined.wait()
                    asyncio.create_task(
                        self.run_phase("flood", self.flood(int(words[1]), float(words[2])))
                    )
                elif words[0] == "probe":
                    await self.joined.wait()
                    asyncio.create_task(self.run_phase("probe", self.probe(float(words[1]))))
                elif words[0] == "wait":
                    await asyncio.sleep(0)
                    await self.phase_done.wait()
                    async with self.busy:
                        pass
                elif words[0] == "joined":
                    await self.joined.wait()
                elif words[0] == "stats":
                    reply = json.dumps(self.stats.summary(), ensure_ascii=False)
                elif words[0] == "quit":
                    writer.write(b"ok\n")
                    await writer.drain()
                    asyncio.get_running_loop().stop()
                    return
                else:
                    reply = "error unknown command"
                writer.write((reply + "\n").encode())
                await writer.drain()
        finally:
            writer.close()


async def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--port", type=int, default=16667)
    parser.add_argument("--control-port", type=int, default=16668)
    parser.add_argument("--members", type=int, default=50, help="NAMES entries per channel")
    parser.add_argument("--expect-channels", type=int, default=1)
    parser.add_argument("--marker-timeout", type=float, default=120.0)
    parser.add_argument("--seed", type=int, default=1)
    parser.add_argument("--image-every", type=int, default=0,
                        help="every Nth message carries an image link (0: none)")
    parser.add_argument("--image-links", type=int, default=120, help="distinct image links")
    args = parser.parse_args()
    server = Server(args)
    irc = await asyncio.start_server(server.handle_client, "127.0.0.1", args.port)
    control = await asyncio.start_server(server.control, "127.0.0.1", args.control_port)
    print(f"listening on 127.0.0.1:{args.port} (control {args.control_port})", file=sys.stderr)
    async with irc, control:
        await asyncio.Event().wait()


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except (KeyboardInterrupt, RuntimeError):
        pass
