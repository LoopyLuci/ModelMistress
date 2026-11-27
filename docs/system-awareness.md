# System awareness

ModelMistress runs heavy work (model servers, Forge training, image and video generation, quantizing) on a PC that is also
somebody's desk. System awareness keeps the two from fighting: the hub looks at the machine every two seconds, decides how
pressed it is, and adjusts what it started, live, in both directions. It is on by default (`mode = balanced`) and lives in
`src/sysaware/`.

It works without any other program. [SystemWatchdog](https://github.com/LoopyLuci/SystemWatchdog) is optional: when it runs
it adds the readings the hub cannot take itself (see "What it looks at").

## What it looks at

| Reading | From | Judged as |
|---|---|---|
| CPU used by **other programs** (the machine's total less the hub's own tree) | the hub (`sysinfo`) | busy 60%, strained 80%, critical 93% |
| Free physical memory | the hub | busy under 15% free, strained under 8%, critical under 4% |
| Windows commit headroom (a GPU load is charged against it, see `serving.md`) | the hub | busy under 12 GiB, strained under 6, critical under 3 |
| Free video memory | Watchdog, or the hub's own GPU probe when it ran in the last minute | busy under 4 GiB, strained under 2 GiB or the hub's `vram_reserve_mib` (the larger), critical under 512 MiB |
| CPU and GPU temperature | Watchdog | strained 88 C / 92 C, critical 95 C / 102 C |
| GPU load, drive activity | Watchdog | busy at 90% (these two never go above busy) |
| A full-screen game, a full-screen program or a presentation | the hub (the shell's notification state) | at least strained (`yield_to_fullscreen`) |

A reading nobody took is never judged. Only other programs' CPU counts: the hub's own work does not make the machine look
"busy" to itself, or it would slow itself down for using the machine. The level is the worst of the readings, and
`system.status` lists every reason with its numbers ("other programs use 86% of the CPU (limit 80%), mostly game.exe (60%)").

**Levels:** `calm`, `busy`, `strained`, `critical`. A level rises after two looks in a row above it and falls one step after
five looks in a row below it, so a reading that flickers around a limit changes nothing and a brief spike does not pause
anything.

## What it does about it

The rules are a table (`src/sysaware/policy.rs`); the governor applies the answer and later undoes it.

| Process | calm / busy | strained | critical |
|---|---|---|---|
| **Worker** (python, sd-cli, ffmpeg, llama-quantize, llama-bench...) | below normal | idle priority + Windows efficiency mode | the same, and **paused** once the machine has been critical for `pause_after_s` (20 s), for at most `max_pause_s` (15 min) |
| **Model server** (llama-server and relatives) | left alone | below normal | below normal |
| **Anything else** the hub started (a command Misty ran) | left alone | below normal | idle priority |

Modes: `off` changes nothing and puts back what it changed; `gentle` lowers workers only and never the model servers;
`balanced` is the table above; `strict` acts one level earlier. `background_priority = "normal"` leaves calm workers alone.

What the governor promises, each with a test:

- It only **lowers**. A process already at or below the cap is not touched, and a priority is never raised above what the
  process had.
- It only touches **processes the hub started** (descendants of the hub), and names a process by pid *and* start time, so a
  pid that Windows reused for another program is never changed. A parent link is believed only when it can be true: Windows
  keeps a process's parent pid after the parent exits and later gives that pid to another program, so a process that is older
  than the parent it names (or whose start time is unknown) is not the hub's child. This was found on the live hub, where an
  `adb.exe` started hours before the hub was lowered because its old parent pid had been reused by a console host.
- It leaves the operating system's **helpers** alone: the console host (`conhost.exe`, `OpenConsole.exe`) that Windows starts
  for a child, and the crash reporter. Lowering them would only slow the output of the program they serve. Their CPU
  still counts as the hub's own.
- It puts back **exactly** what it changed: when the level falls, when the mode is switched to `off`, and when the hub stops.
- It does not **fight**. If another program (the user in Task Manager, a script) changes a priority the governor set, the
  governor lets go of that process and does not "restore" over the user's choice.
- A pause is **bounded**, never applied to a model server answering chat, and does not come back at the same level after
  `max_pause_s`. A process the sampler lost sight of while paused is resumed, not left suspended.
- A change Windows refuses (access denied) is reported once, not retried every tick.

### Heavy jobs wait

A Forge run, a Knowledge Module factory task, a quantize and an image job (`HEAVY` in `src/sysaware/mod.rs`) do not start
while the machine is `strained` or worse. The job exists at once and shows `waiting for the machine to quiet (42s): other
programs use 86% of the CPU...` as its message. It can be cancelled, `system.run_now` starts it anyway, and `max_wait_s`
(default 0 = as long as it takes) gives up waiting. Downloads, installs and Misty's own jobs do not wait. `mode = off` never
waits.

### Idle models are shed when memory is short

At strained or worse, when memory, commit or video memory is a reason, the hub unloads the model that has been idle longest
(at least `shed_idle_s`, default 120 s), if no request is using it. One model per ten seconds, so the effect of each can be seen
before the next. A model pinned with `keep_alive: -1` is taken only at critical. `shed = false` turns it off. The next request for
that model loads it again.

## Using it

```
model-mistress system                       the level, why, what is held down, who waits, what it did lately
model-mistress system watch                 the same, every two seconds
model-mistress system mode off|gentle|balanced|strict
model-mistress system config                the settings
model-mistress system config thresholds.cpu_strained_pct=85 shed=false
model-mistress system run-now               start the jobs that are waiting
```

Operations (owner only): `system.status` (also in the MCP core set, and `machine.status` carries a short `pressure` block),
`system.config.get`, `system.config.set` (a merge patch; an unknown or out-of-order setting is refused and nothing changes),
`system.run_now`.

Settings are `<home>/system.toml`. A hand edit is taken in on the next look; a broken file keeps the last good settings and
`system.status` shows the error. Without a running hub, `system mode` and `system config` edit the file.

| Setting | Default | |
|---|---|---|
| `mode` | `balanced` | off, gentle, balanced, strict |
| `tick_ms` | 2000 | how often the machine is looked at (250 to 60000) |
| `rise_after`, `fall_after` | 2, 5 | looks before a level rises / falls a step |
| `background_priority` | `below_normal` | where calm workers run (`normal` leaves them alone) |
| `pause_after_s`, `max_pause_s` | 20, 900 | |
| `shed`, `shed_idle_s` | true, 120 | |
| `max_wait_s` | 0 | |
| `watchdog_url`, `watchdog_max_age_s` | `http://127.0.0.1:7781`, 15 | empty `watchdog_url`: do not look for Watchdog |
| `thresholds.*` | see the table above | the limits for each level |

## SystemWatchdog

The hub polls `GET /pressure` on Watchdog's loopback feed each look (900 ms timeout; after a miss it waits 2, then 10, then
30 seconds, so a Watchdog that is not installed costs nothing). `system.status` shows `watchdog`: `connected` (with its version
and the age of the reading), `stale` (it answers but its newest sample is older than `watchdog_max_age_s`), `absent`, or `off`.
In the other direction Watchdog's action engine and core balancer skip the processes that descend from the MM hub, so the two
programs never set one priority against each other (see Watchdog's `docs/modelmistress.md`).

## Limits

- **Windows only changes processes.** Elsewhere the hub still looks, makes heavy jobs wait and sheds idle models, but it
  changes no process (`can_change_processes: false`).
- **GPU work cannot be prioritised per process** with a priority class. What the hub can do for the GPU is not start heavy
  jobs while it is busy, shed idle models, and (with Watchdog) see its load and temperature.
- **Without Watchdog**, GPU load, temperatures and drive activity are not measured, and video memory only when something
  probed the cards in the last minute (a load, `model.plan`, `machine.status`).
- A CPU affinity or thread-count budget is not set; priority and efficiency mode do the work.
- **The look itself costs something.** Reading the process table took about 0.14 s here (the best of six, on a PC with several
  hundred processes, while other programs were running), which at the default two-second look is a few percent of one core.
  `tick_ms` raises the interval (up to 60 s) if that matters more than reacting quickly.
- Efficiency mode is set (the call succeeds) but Windows 10 does not reliably report it back, so the governor trusts its own
  record of what it set.

## What has been run, and what has not

Run on 2026-11-27, Windows 10, in a scratch hub (its own home and port; the hubs in use were not touched) on a copy of the
debug build:

- A real `forge.run` job, through a stand-in `mm_forge` package that burns one core, so the hub started a real Python
  process. Calm: below normal, and it kept the whole core. With the CPU limits set to 5%: idle priority and efficiency mode,
  and after 20 s of critical, paused (121 s in all; the hub resumed it). The level stepped down critical, strained, calm as the
  limits were restored, and the processes were released at the matching steps. Switching the mode to `off` while the process
  ran put it back from below normal to normal.
- With a hot GPU reported by Watchdog's feed (strained): a `forge.run` job waited with the message "waiting for the machine to
  quiet (5s): the GPU is at 95 C (limit 92 C)" and started no process; cancelling it said "cancelled while it waited for the
  machine to quiet"; a second job started with `system run-now` and was lowered to idle with efficiency mode while it
  still used a full core of spare CPU.
- Watchdog's own `PressureFeed` class on a scratch port with synthetic readings: the hub showed `connected (v0.2.2)`, took in
  GPU load, temperatures and video memory, called the feed `stale` when it stopped publishing and dropped its readings, and
  connected 0.6 s after `watchdog_url` was changed. Watchdog refused a wrong Host header (403) and a POST (405).
- Two faults the run found, fixed and covered by tests: a console host was lowered like a program, and a changed
  `watchdog_url` waited out the backoff earned at the old address.
- A third fault, found on the live hub after it was restarted on this build: an `adb.exe` that predates the hub was lowered
  to idle during a critical spell, through a reused parent pid (see "What the governor promises"). Fixed by the start-time
  check on every link of the walk, with four tests; the priority was put back when the level fell.

Tested without a model: idle-model shedding (the engine's `shed_one` and the loop's decision, including that CPU pressure and
`shed = false` shed nothing), on stand-in instances.

**Not run:** a real model server being lowered or a real model being shed; a Forge run on the GPU; the installed Watchdog
(the old version, not restarted) feeding real readings; the live hubs; Linux and macOS (where the hub changes no process).
