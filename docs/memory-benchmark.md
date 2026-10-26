# Memory benchmark: mhc vs OmO

This page shows how much memory mhc and OmO (oh-my-openagent on senpi) use on the same multi-agent
workflow, how that was measured, and how to rerun it.

## Result

Both agents ran the same fixed workflow DAG (a 4-node DAG with three parallel child agents and a
join), each in its own fresh Docker container. Each agent got 1 warm-up run and then 3 measured
runs, and every measured run finished the DAG and wrote its output. All figures cover the whole
container: the agent itself, every child agent and every subprocess.

| Per run (min-max over 3 runs) | mhc | OmO | OmO / mhc |
|---|---|---|---|
| Anonymous memory, peak | **83-86 MB** | **1356-1398 MB** | **~16x** |
| Anonymous memory, mean over the run | 70-73 MB | 894-931 MB | ~13x |
| Page cache, peak | 69 MB | 246-279 MB | ~4x |
| `memory.peak` (anon + cache + kernel) | 158-161 MB | 1629-1668 MB | ~10x |
| Wall time | 88-110 s | 94-107 s | about equal |
| Processes, peak | 7 | 13 | |

Measured on 2026-10-19, Linux x86_64 (AMD Ryzen 5 5600X, cgroup v2, Docker 29.7), mhc
`2026.10.19-2` (main at `bab5f62a`) and OmO 5.1.35 (senpi engine 2026.10.16). Both used the same
`~/.omo` configuration, the same provider endpoint, and the same models: `claude-opus-5-5` in the
main session and `gemini-3.8-flash-high` for the `quick` category children.

Anonymous memory is the number to compare. It counts what the processes allocated themselves
(heaps, stacks, JIT code). Page cache counts files the kernel kept in memory after they were read
(the mhc binary, OmO's `node_modules` and JS bundles), and the kernel can drop it under pressure.

Runs were consistent: each agent's anon peak varied by at most 3% across its three runs.

These figures are historical: they were measured on 2026-10-19 with an earlier version of the
prompt that also told the agent to load the since-retired `mass-ulw` skill before running the DAG.
The DAG, the inputs and the measurement were the same as the method below describes; the current
prompt drops only that skill instruction. Rerun the benchmark for numbers on the current build.

## Why the gap

- **One process vs one runtime per child.** mhc runs the main session and every DAG child as
  in-process sessions in one native binary. OmO starts each child agent as its own Node.js process
  (`env node .../senpi/dist/bundle/cli.js`), and each of those loads the full senpi runtime and
  extension set. Before starting a child, OmO also runs a short-lived Node process that checks the
  model catalog. That is why OmO peaks at 13 processes and mhc at 7.
- **Fixed cost per JS runtime.** Each Node process carries its own V8 heap, compiled bundle and
  extension state, so OmO's cost grows with the number of children that are alive at the same
  time. mhc's children share one address space and one copy of the code.
- **Time is the same.** Both agents spend almost all of their time waiting on the model. A
  smaller memory footprint here does not change how long the work takes.

## What this does and does not show

- The workflow is small on purpose: each child summarizes a 40-line file. Real work (builds, large
  files, long contexts) raises both numbers. The per-child runtime overhead is structural, so the
  gap is not expected to close, but these runs do not measure that.
- Three runs per agent is enough to show the spread is small. It is not a statistical study.
- Model output varies from run to run (turn count, tool calls), so time and memory vary slightly
  even with identical inputs.
- OmO ran on the host's own Node (v26.8.1) and bun install, mounted into the container, so it is
  OmO as installed on that machine, not a separately tuned setup.

## Method

### The workflow

Each run gives the agent the same prompt in non-interactive mode (`-p`):

```text
workflow 도구로 아래 고정 DAG를 정확히 한 번 실행해. 노드를 추가하거나 바꾸지 마.
- a: /bench/work/a.txt 를 읽고 한 문장으로 요약 (category quick)
- b: /bench/work/b.txt 를 읽고 한 문장으로 요약 (category quick)
- c: /bench/work/c.txt 를 읽고 한 문장으로 요약 (category quick)
- join (dependsOn a,b,c): 세 요약을 한 줄로 합쳐 /bench/work/out.txt 에 써 (category quick)
완료되면 "DONE" 한 줄만 출력해.
```

The prompt asks the agent to run this fixed DAG through the workflow tool
exactly once, without adding or changing nodes, and to print "DONE" when it finishes.

Each input file is 40 lines (`a line 1` ... `a line 40`). The agent starts
the DAG with the `workflow` tool, three child agents (a, b, c) run in parallel, a fourth (join) runs
after them, and the main session checks the result. A run counts only if `/bench/work/out.txt`
exists and is non-empty afterwards (`out_ok=yes`).

### Isolation

Every run gets a new container, so the container's cgroup holds exactly that run:

- `/sys/fs/cgroup` inside the container is the container's own cgroup. Its `memory.peak` and
  `memory.stat` include every process the agent starts, child agents included.
- Nothing outside the container can do work for the run. This matters for OmO: on a normal host it
  can hand sessions to a long-lived shared RPC host daemon, whose memory would not appear in a
  per-process measurement.
- Each run gets a private writable copy of the config home (`~/.omo`) and of `~/.bun`. Copies are
  reflinks where the filesystem supports them, so they are cheap. `$HOME`, `/tmp` and the work
  directory are fresh tmpfs mounts.
- The containers use host networking, so both agents reach the same provider endpoint.

### Warm-up

An agent's first start in a fresh home migrates config, clones extension repositories and fills
caches. The warm-up run absorbs that and its numbers are discarded. Each measured run starts from a
copy of the warmed home, with the previous run's sessions and DAG state removed.

### Sampling

`run-in-container.sh` records, every 0.2 s:

| Field | Source |
|---|---|
| total | `memory.current` |
| anon | `anon` in `memory.stat` |
| file | `file` in `memory.stat` |
| processes | line count of `cgroup.procs` |

After the agent exits it records `memory.peak`, the kernel's own high-water mark, which catches
spikes shorter than the sampling interval. Anon and file peaks come from the 0.2 s samples, so a
very short spike between samples can be missed. `memory.peak` cannot miss it.

## Reproducing

### Requirements

- Linux with cgroup v2 and Docker.
- mhc installed (`~/.local/share/mhc/mhc`, the layout `install.sh` creates).
- OmO installed through bun (`bun add -g omo-ai`, giving `~/.bun/bin/omo`).
- Node.js on `PATH`. OmO starts its child agents with `node`.
- A working `~/.omo` config with models and auth that both agents can use (mhc reads the same
  files as OmO), and a model configured for the `quick` category.
- Roughly 3 GB of free disk for the config and bun copies (less with reflinks).

### Run

```bash
tools/membench/bench.sh
```

This builds a small image (`tools/membench/Dockerfile`), does 1 warm-up and 3 measured runs per
agent (alternating mhc and OmO), and prints a table:

```text
run        rc  secs    peak anon_peak anon_avg file_peak procs  ok
mhc-1       0   110     158        83       70        69     7 yes
mhc-2       0   103     161        86       73        69     7 yes
mhc-3       0    88     161        86       71        69     7 yes
omo-1       0    99    1668      1356      894       279    13 yes
omo-2       0    94    1629      1372      907       248    13 yes
omo-3       0   107    1647      1398      931       246    13 yes
```

Memory columns are in MB. A full run takes about 20-30 minutes and makes 8 agent sessions' worth of
model calls.

| Variable | Default | Meaning |
|---|---|---|
| `RUNS` | `3` | measured runs per agent |
| `AGENTS` | `mhc omo` | which agents to run |
| `WORK` | `~/.cache/mhc-membench` | scratch directory; results go to `$WORK/out` |
| `MHC_DIR` | `~/.local/share/mhc` | directory containing the `mhc` binary |
| `BUN_DIR` | `~/.bun` | bun install that has `omo-ai` installed globally |
| `NODE_DIR` | install prefix of `node` on `PATH` | Node install mounted into the container |
| `OMO_HOME` | `~/.omo` | config home to copy |

Raw data for each run stays in `$WORK/out`: `<run>.samples` (one line per sample: epoch,
`memory.current`, anon, file, process count; memory in bytes), `<run>.out` (the agent's output) and
`<run>.result`. To print the table again:

```bash
tools/membench/summarize.sh ~/.cache/mhc-membench/out
```

### Troubleshooting

A run that ends with `ok=no` did not finish the DAG, and its memory numbers are not comparable.
Check `$WORK/out/<run>.out`. These are the failures seen while building this setup, all specific
to running OmO in a container:

| Symptom in the OmO output | Cause | What `bench.sh` does about it |
|---|---|---|
| `EROFS accessing temporary directory` | bun cannot write its temp dir in a read-only `~/.bun` | mounts a writable copy of `~/.bun` |
| `EACCES: mkdir '~/.senpi'` | `$HOME` not writable | `$HOME` is a writable tmpfs |
| Children fail with `model catalog probe failed` | The probe subprocess cannot find the config (no `SENPI_CODING_AGENT_DIR`) or cannot start (no `node`) | sets `SENPI_CODING_AGENT_DIR`/`OMO_CODING_AGENT_DIR` and mounts the host Node |
| Re-clones extensions on every start | `~/.omo/agent/git` missing | copies it into the template home |

`model catalog probe failed` hides the real error. To see it, run the probe by hand in the same
container setup:

```bash
node ~/.bun/install/global/node_modules/@code-yeongyu/senpi/dist/bundle/cli.js \
  --no-extensions --no-skills --no-prompt-templates --no-context-files --list-models
```

## Measuring without Docker

For a quick look on the host, run the agent in a transient systemd scope and read its cgroup:

```bash
systemd-run --user --scope --unit=membench-mhc -p MemoryAccounting=yes \
  bash -c 'mhc -p "<prompt>"; cat /sys/fs/cgroup$(cut -d: -f3 /proc/self/cgroup)/memory.peak'
```

This undercounts OmO whenever it hands work to an RPC host daemon that is already running, because
that daemon lives in a different cgroup. Stop any running OmO host first, or use the Docker setup
above.

## Files

| File | Purpose |
|---|---|
| [`tools/membench/bench.sh`](../tools/membench/bench.sh) | builds the image, prepares homes, runs warm-up and measured runs |
| [`tools/membench/run-in-container.sh`](../tools/membench/run-in-container.sh) | runs inside each container: input files, sampler, the agent, result line |
| [`tools/membench/summarize.sh`](../tools/membench/summarize.sh) | turns `$WORK/out` into the results table |
| [`tools/membench/Dockerfile`](../tools/membench/Dockerfile) | minimal base image; everything else is mounted from the host |
