# The web UI: `fvol serve`

`fvol serve` runs a small web server inside the `fvol` binary and serves an analysis workspace in
the browser: open a memory dump, run sets of plugins, read their results and keep all of it for
next time. It is part of fastvol only; python volatility3 has no equivalent command.

This page has two parts. The [tutorial](#tutorial-a-first-session) walks through a first
session. The [reference](#reference) lists the options, the saved files, the keyboard
shortcuts, the HTTP API and the security model.

Applies to fastvol 0.1.0.

## Tutorial: a first session

In this tutorial you open a Windows image, run a preset, read the results, save your own preset
and come back to the analysis later. You need a built `fvol` binary, a memory image and a browser
on the same machine. Linux and macOS images work the same way once their symbol directory is set
in **Options** (or given with `-s`).

### 1. Start the server

```bash
fvol serve
```

```text
fastvol web UI · Volatility 3 Framework 2.28.2
  image   (none yet: open one from the UI)
  output  /cases/vol-serve-output
  open    http://127.0.0.1:8765/#token=1c5fa1176bd21e349985b1160bfc6ac6
Anyone with this URL can read the image. Press Ctrl+C to stop.
```

Copy the `open` URL into your browser. The page stores the token and removes it from the address
bar.

### 2. Open a memory image

The page starts with the **Quick Start**:

- **Continue** takes the image `fvol serve -f` was given.
- **New analysis** browses the server's folders. Memory images are tagged *image*; double-click
  one, or press **Open File…** to pick it in your desktop's own file dialog.
- **Previous** lists the dumps you analysed before (see step 7).

Press `Esc` to go straight to the workspace.

### 3. Read the workspace

The workspace has four panels, each resizable by dragging the lines between them
(double-click a line to reset it):

- **File Overview** on the left prints what fastvol found about the image, line by line: path,
  size, operating system, capture time, symbols, and the kernel facts of `windows.info`.
- **Plugins** in the middle is where plugins are chosen.
- **Runs** at the top right lists the runs of this image.
- **Triage Hints** at the bottom right flags processes worth a look: unexpected parents,
  duplicated singletons, processes started shortly before capture. **Load Rules…** adds your
  own checks from a JSON file ([rules](#triage-rules)).

In a narrow window the File Overview hides first; the **File Overview** button in the top bar
shows it again.

### 4. Run a preset

Click **Presets** in the Plugins header and choose **Windows Triage**. Its plugins are ticked
under **Select**, where you can untick some or tick more. A ticked plugin with options unfolds
them below it: checkboxes for flags such as `--dump`, boxes for values such as `--pid`.

Press **Run**. All the ticked plugins start together as one run, named after the preset, a few
at a time. The run appears under **Runs** with its progress, and its results replace the File
Overview and Plugins panels.

### 5. Read the results

The results panel, *Windows Triage: Results*, has a tab per plugin with its status and row count.
Step through them with `]` and `[`, or `Ctrl+→` and `Ctrl+←`.

- **Filter rows** filters the rows of every plugin of the run: type `svchost` once, then step
  through the tabs. Each run keeps its own filter.
- **Filters** adds a filter box per column: `text`, `=exact`, `!not`, `>0x10`, `/regex/`.
- Click a column header to sort, drag it sideways to move the column, drag its right edge to
  resize it and double-click that edge to fit the column to its values.
- Plugins that nest processes, such as `pstree`, draw their tree in the first column.
- **Export** downloads the rows shown as CSV, TSV, JSON, JSON Lines or Markdown, or as
  **fvol output**: exactly what `fvol` prints for the plugin.

Press `Esc` or **×** to go back to the File Overview and Plugins. Click a run under **Runs** to
open its results again; click it once more to fold it.

### 6. Save a preset

Under **Select**, tick the plugins you want and press **Save Preset**. Name it; it is saved as
`~/.fvol/presets/<name>.json` and listed under **Presets** for every image from then on.

### 7. Come back later

Stop the server with `Ctrl+C`. Everything you did is already saved: the runs, their results and
filters, the options and the rules. Start `fvol serve` again, choose **Previous** in the Quick
Start and pick the dump. The fastvol caches make the image open immediately, and the results
come back without running any plugin again. To delete an analysis, press **Delete** next to it
in **Previous**.

## Reference

### Command line

```text
fvol serve [-h] [-f FILE] [--host HOST] [--port PORT] [-s SYMBOL_DIRS] [-o OUTPUT_DIR]
           [--offline] [-u URL] [--cache-path PATH] [--token TOKEN] [--allow-host NAME]
           [--max-conns N] [--parallel N] [--max-memory SIZE]
```

| Option                   | Default              | Meaning                                                                  |
| ------------------------ | -------------------- | ------------------------------------------------------------------------ |
| `-f, --file FILE`        | none                 | Image to open. Without it, open one from the Quick Start.                |
| `--host HOST`            | `127.0.0.1`          | IP address to listen on. `localhost` means `127.0.0.1`.                  |
| `--port PORT`            | 8765                 | Port. Without the option, the first free port from 8765 to 8784 is used, then any free port. `0` means any free port. |
| `-s, --symbol-dirs DIRS` | none                 | Semicolon-separated symbol directories, as for `fvol`. Like `--offline`, `-u` and `--cache-path`, it wins over the options saved with an analysis. |
| `-o, --output-dir DIR`   | `./vol-serve-output` | Root of the per-run output directories. Given, it wins over an output folder saved with an analysis. |
| `--offline`              | off                  | Never download symbols.                                                  |
| `-u, --remote-isf-url URL` | none               | Remote symbol file list, as for `fvol`.                                   |
| `--cache-path PATH`      | python's default     | python volatility3 cache path, as for `fvol`.                             |
| `--token TOKEN`          | random               | Fixed access token: at least 16 printable characters, without `;`, `,` or quotes. |
| `--allow-host NAME`      | none                 | Also accept requests whose `Host` header is `NAME`, for example behind a reverse proxy. Repeatable. |
| `--max-conns N`          | 512                  | Concurrent HTTP connections, between 8 and 4096.                         |
| `--parallel N`           | 3                    | Plugins that may run at the same time, between 1 and 64.                 |
| `--max-memory SIZE`      | `3G`                 | Memory for stored result rows across all runs, such as `2G` or `512M`. Rows past the budget are counted but not kept; exports through the `fvol` renderer stay complete. |

### Options

**Options** in the top bar sets the global options of `fvol`. Tick an option to use it; each does
what it does on the command line, applied where the web UI has the same step. The options are
saved with the analysis.

| Options                                                                         | Applied                                                                          |
| ------------------------------------------------------------------------------- | -------------------------------------------------------------------------------- |
| `-s`, `--offline`, `-u`, `--cache-path`, `--single-location`, `--stackers`, `--single-swap-locations`, `-v`, `-o` | When the image is opened. A change reopens the image; its runs stay. |
| `--clear-cache`                                                                 | Once, when the image reopens. It empties `~/.cache/fastvol` only, never `~/.fvol`. |
| `-c`, `-e`                                                                      | To each plugin started afterwards: values for options the plugin leaves unset. `-e automagic.*` keys act when the image is opened. |
| `--write-config`, `--save-config NAME`                                          | Each run writes its configuration, as `fvol` writes it, into its output folder (`config.json`, or `NAME`). |
| `-l FILE`                                                                       | A line per run started, finished or failed. `FILE` must end in `.log`.            |
| `--parallelism off`                                                             | One plugin at a time instead of `--parallel N`.                                  |
| `-r`, `--filters`, `--hide-columns`                                             | The **fvol output** export.                                                       |
| `-q`, `-p`                                                                      | No effect: the web UI has no console progress, and fastvol does not load python plugins. |

### Saved files

```text
~/.fvol/<dump_id>-metadata.json        one analysis: the dump's path, size and modification time,
                                       the options, the rules file, every run and its plugins
~/.fvol/<dump_id>/<run>/<n>.jsonl      the rows of the n-th plugin of a run
~/.fvol/presets/<name>.json            your presets, for every image
```

`dump_id` is the key fastvol's cache (`~/.cache/fastvol`) uses for an image: a hash of its
canonical path, size and modification time. A moved or changed dump is therefore a new
analysis; **Previous** marks the old one *missing* or *changed*. A metadata file this fastvol
cannot read (damaged, or written by another version) is listed as *unreadable* and left as it is:
the dump opens without its analysis, and nothing is saved over the file until it is deleted.
Opening another dump saves the current one and closes its runs. The dump itself is never
copied. **Delete** next to an analysis in **Previous** removes its metadata and result rows (for
the dump that is open, its runs too). Results that did not fit the memory budget (`--max-memory`) are not saved; their run
lists them, and running the plugin again gets them back.

A preset file:

```json
{"name": "Ransomware sweep", "os": "windows", "created": 1790000000,
 "plugins": [{"plugin": "windows.pslist.PsList", "args": {"pid": "4 628"}},
             {"plugin": "windows.malware.malfind.Malfind", "args": {"dump": true}}]}
```

Presets can be copied between machines and edited by hand; a file that cannot be read is listed
with the reason under **Presets**.

### Triage rules

**Load Rules…** reads a JSON file of checks against the process list. A rule flags the processes
for which every field of its `match` matches:

```json
{"name": "Office and script abuse",
 "rules": [{"title": "Office app spawning a shell", "severity": "high",
            "description": "Macros often launch cmd or PowerShell.",
            "match": {"parent": "/^(winword|excel|outlook)\\.exe$/i",
                      "name": "/^(cmd|powershell|wscript)\\.exe$/i"}}]}
```

| Field | Value |
| ----- | ----- |
| `name`, `parent`, `path`, `cmdline` | text: `"text"` (contains), `"=exact"`, `"!not"`, `"/regex/flags"` |
| `pid`, `ppid`, `threads`, `handles`, `session` | numbers: `">10"`, `"<=0x40"`, `"=4"` |
| `age` | seconds between the process start and the capture, such as `"<600"` |
| `exited`, `wow64` | `true` or `false` |

`severity` is `high`, `medium` (the default) or `low`. `path` and `cmdline` are known on Windows
once `pstree` ran in the background. The rules file is saved with the analysis.

### Output files

Every run of a plugin that writes files gets its own directory below the output root, named
`run-<NNNN>-<plugin>`; a run's configuration file (`--write-config`) goes there too. Exports
through the `fvol` renderer use `export-<NNNN>-<N>`. Downloads are served only from these
directories.

### Keyboard shortcuts

| Where          | Keys                         | Action                                                   |
| -------------- | ---------------------------- | -------------------------------------------------------- |
| Quick Start    | `1` to `3`                   | Choose                                                   |
|                | `Esc`                        | Go to the workspace                                      |
| Anywhere       | `Shift+T`                    | Light or dark theme                                      |
| Plugins        | `↑`, `↓`, `Space`, `Enter`   | Move, tick                                               |
| Runs           | `Enter`                      | Open the run's results (again: fold it)                  |
|                | `Space`, `←`, `→`            | Fold or unfold                                           |
|                | `F2`                         | Rename                                                   |
| Results        | `]`, `[`, `Ctrl+→`, `Ctrl+←` | Next and previous plugin                                 |
|                | `/`                          | Filter rows                                              |
|                | `Esc`                        | Close the results                                        |
| Result table   | arrows, `PgUp`, `PgDn`       | Move; `Ctrl+Home` and `Ctrl+End` go to the first and last row |
|                | `C`, `Shift+C`               | Copy the cell, or the row as TSV                         |
|                | `S`, `Shift+S`               | Sort by the column; again to reverse; with Shift, add a sort key |
|                | `F`                          | Filter the column                                        |
| Splitters      | arrows                       | Resize                                                   |

### HTTP API

The API exists for the UI and for scripts. Every call under `/api/` needs the token in an
`X-Vol-Token` header or an `Authorization: Bearer <TOKEN>` header. Bodies are JSON.

| Method and path                      | Purpose                                                            |
| ------------------------------------ | ------------------------------------------------------------------ |
| `GET /api/session`                   | Current image, analysis state, OS, and the facts of the overview   |
| `POST /api/session`                  | Open another image: `{"file": "<PATH>", "symbol_dirs": ["<DIR>"]}` (`symbol_dirs` optional, set like `-s` in Options); its saved analysis comes back |
| `GET /api/plugins`                   | Every plugin with its options                                      |
| `GET /api/batches`                   | The runs (each a set of plugins started together)                  |
| `POST /api/batches`                  | Start a run: `{"name": "<NAME>", "entries": [{"plugin": "<NAME>", "args": {"<option>": <value>}}]}`; every entry is checked first |
| `POST /api/batches/<ID>/name`        | Rename a run: `{"name": "<NAME>"}`                                 |
| `POST /api/batches/<ID>/filter`      | Set a run's **Filter rows** text: `{"q": "<TEXT>"}`                |
| `POST /api/batches/<ID>/cancel`      | Cancel the run's plugins still queued or running                   |
| `DELETE /api/batches/<ID>`           | Remove a run and its results                                       |
| `GET /api/runs`                      | Every plugin execution                                             |
| `POST /api/runs`                     | Start one plugin on its own: `{"plugin": "<NAME>", "args": {...}}`  |
| `GET /api/runs/<ID>`                 | One plugin's status and columns                                    |
| `DELETE /api/runs/<ID>`              | Remove a plugin execution                                          |
| `POST /api/runs/<ID>/cancel`         | Cancel a plugin                                                    |
| `POST /api/runs/<ID>/view`           | Create a sorted and filtered view of the rows                      |
| `GET /api/runs/<ID>/rows`            | A page of rows: `from`, `count` up to 5000, optional `view`        |
| `GET /api/runs/<ID>/stream`          | Every row as NDJSON while the plugin produces it                   |
| `GET /api/runs/<ID>/export`          | The rows as `format=csv`, `tsv`, `json`, `jsonl` or `md`           |
| `GET /api/runs/<ID>/vol`             | The plugin's `fvol` output; `renderer=<NAME>`, else the one in Options, else `quick`, as `fvol` |
| `GET /api/runs/<ID>/files`           | Files the plugin wrote                                             |
| `GET /api/runs/<ID>/files/<NAME>`    | Download one file                                                  |
| `GET /api/runs/<ID>/files.zip`       | Download all files as a zip                                        |
| `GET /api/options`, `POST /api/options` | The global options ([Options](#options)); the answer says whether the image was reopened |
| `GET /api/rules`, `POST /api/rules`, `DELETE /api/rules` | The triage rules file: `{"file": "<NAME>", "text": "<JSON>"}` |
| `GET /api/analyses`                  | The saved analyses, newest first, with whether each dump is still there |
| `DELETE /api/analyses/<DUMP_ID>`     | Delete a saved analysis (its metadata and result rows; never the dump). For the open dump, its runs go too |
| `GET /api/presets`, `POST /api/presets`, `DELETE /api/presets/<ID>` | Your presets in `~/.fvol/presets` |
| `GET /api/pick-file`, `POST /api/pick-file` | Whether the desktop file dialog is available; show it: `{"dir": "<START>"}` |
| `GET /api/mem`                       | Read memory: `layer` = `phys`, `kernel` or `pid:<PID>`, `addr`, `len` up to 256 KiB |
| `GET /api/disasm`                    | Disassemble: `layer`, `addr`, `len` up to 16 KiB, optional `arch`  |
| `GET /api/fs`                        | List a directory for the Quick Start: names and sizes              |
| `POST /api/ticket`                   | A single-use link for one GET URL, valid for 60 seconds            |
| `GET /api/events`                    | NDJSON stream of session, run and plugin changes                   |
| `GET /api/stats`                     | Run count and memory use                                           |

Example: run two plugins as one run and fetch the command-line output of the first.

```bash
TOKEN=<TOKEN>
curl -s -H "X-Vol-Token: $TOKEN" -X POST \
  -d '{"name": "Quick look", "entries": [{"plugin": "windows.pslist.PsList", "args": {"pid": [4]}}, {"plugin": "windows.info.Info"}]}' \
  http://127.0.0.1:8765/api/batches
curl -s -H "X-Vol-Token: $TOKEN" "http://127.0.0.1:8765/api/runs/1/vol?renderer=csv"
```

```text
TreeDepth,PID,PPID,ImageFileName,Offset(V),Threads,Handles,SessionId,Wow64,CreateTime,ExitTime,File output
0,4,0,System,0xe485b4eaa040,134,-,N/A,False,2026-09-14 02:53:44.000000 UTC,N/A,Disabled
```

The `vol` endpoint runs the plugin again with the command-line renderer, so its output is the
same as `fvol -r csv` would print.

### Developing the UI

The UI is hand-written HTML, CSS and JavaScript in `src/web/assets/`, compiled into the binary.
With `FASTVOL_WEB_DEV=<DIR>` set, `fvol serve` reads the page and its assets from `<DIR>` on
every request instead, so an edit shows on a browser refresh without rebuilding:

```bash
FASTVOL_WEB_DEV=$PWD/src/web/assets fvol serve -f <IMAGE>
```

### Security model

A memory image holds passwords, keys and private data, so the server is built to be reachable
only by the person who started it.

- **Loopback by default.** The server listens on 127.0.0.1. With `--host 0.0.0.0` or another
  address, it prints a warning: traffic is plain HTTP, so anyone on the network path can read it.
  To reach a remote server, prefer an SSH tunnel such as `ssh -L 8765:127.0.0.1:8765 <HOST>`.
- **Access token.** A random 128-bit token from `/dev/urandom` is required on every API call, in
  a request header. There is no cookie, because cookies are shared between all ports of a host.
  The token is compared in constant time, and repeated failures are slowed down.
- **Token handling in the browser.** The token travels in the URL fragment, which browsers never
  send to servers or put in `Referer` headers. The page moves it into the browser storage of that
  exact origin and removes it from the address bar. Downloads that cannot carry a header use
  single-use tickets that expire after 60 seconds and are bound to one URL.
- **DNS rebinding.** Requests are accepted only when the `Host` header names the address the
  server listens on or `localhost`, plus any `--allow-host` names. Other names get status 421.
- **Cross-site requests.** Requests marked by the browser as cross-site, or with a foreign
  `Origin`, are refused. The server sends no CORS headers, and pages are served with a strict
  Content Security Policy, `X-Frame-Options: DENY` and `Referrer-Policy: no-referrer`.
- **No external content.** The UI's HTML, JavaScript and CSS are compiled into the binary.
  The page loads nothing from other hosts.
- **Request limits.** Request heads are limited to 16 KiB and 64 headers, bodies to 1 MiB and JSON
  nesting to 32 levels. A request must arrive within 10 seconds, idle connections close after 30
  seconds, and long-lived streams have their own cap.
- **Files.** Plugins write only into their run's output directory, and downloads are limited to
  names in a fresh listing of that directory: no paths, no `..`, no symbolic links.
- **Saved data.** `~/.fvol` is created readable by its owner only (0700), and its files are
  written 0600 through a temporary name and a rename.
- **The desktop file dialog.** **Open File…** asks the server to show `kdialog` or `zenity` on its
  own display. It is offered only when the server listens on a loopback address and has a
  display, and only one dialog can be open at a time.

What the token grants: whoever holds it can read the open image and can open any other file that
the server's user can read, list directories and read that file's bytes through the memory API.
It can also read and change the saved analyses, presets and rules in `~/.fvol`, and set the
options, which name files the server reads (`-c`) or writes: `--save-config` only into a run's
output directory, and `-l` only to a regular file whose name ends in `.log`, one line per run
with control characters replaced.
Treat the URL like a password, and run `fvol serve` as a user that can read only what you intend
to analyse.
