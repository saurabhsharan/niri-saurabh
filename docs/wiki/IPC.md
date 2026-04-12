You can communicate with the running niri instance over an IPC socket.
Check `niri msg --help` for available commands.

The `--json` flag prints the response in JSON, rather than formatted.
For example, `niri msg --json outputs`.

> [!TIP]
> If you're getting parsing errors from `niri msg` after upgrading niri, make sure that you've restarted niri itself.
> You might be trying to run a newer `niri msg` against an older `niri` compositor.

### Event Stream

<sup>Since: 0.1.9</sup>

While most niri IPC requests return a single response, the event stream request will make niri continuously stream events into the IPC connection until it is closed.
This is useful for implementing various bars and indicators that update as soon as something happens, without continuous polling.

The event stream IPC is designed to give you the complete current state up-front, then follow up with updates to that state.
This way, your state can never "desync" from niri, and you don't need to make any other IPC information requests.

Where reasonable, event stream state updates are atomic, though this is not always the case.
For example, a window may end up with a workspace id for a workspace that had already been removed.
This can happen if the corresponding workspaces-changed event arrives before the corresponding window-changed event.

To get a taste of the events, run `niri msg event-stream`.
Though, this is more of a debug function than anything.
You can get raw events from `niri msg --json event-stream`, or by connecting to the niri socket and requesting an event stream manually.

You can find the full list of events along with documentation [here](https://niri-wm.github.io/niri/niri_ipc/enum.Event.html).

### Programmatic Access

`niri msg --json` is a thin wrapper over writing and reading to a socket.
When implementing more complex scripts and modules, you're encouraged to access the socket directly.

Connect to the UNIX domain socket located at `$NIRI_SOCKET` in the filesystem.
Write your request encoded in JSON on a single line, followed by a newline character, or by flushing and shutting down the write end of the connection.
Read the reply as JSON, also on a single line.

You can use `socat` to test communicating with niri directly:

```sh
$ socat STDIO "$NIRI_SOCKET"
"FocusedWindow"
{"Ok":{"FocusedWindow":{"id":12,"title":"t socat STDIO /run/u ~","app_id":"Alacritty","workspace_id":6,"is_focused":true}}}
```

The reply is an `Ok` or an `Err` wrapping the same JSON object as you get from `niri msg --json`.

#### Focused Window Geometry

`niri msg focused-window` includes the focused window's global screen geometry in the human-readable `Layout` section:

```text
    Global screen geometry: 1920, 47 1280 x 720
```

In JSON, the same data appears only on the focused-window response at `layout.global_screen_geometry`:

```json
{
  "layout": {
    "global_screen_geometry": {
      "x": 1920.0,
      "y": 47.0,
      "width": 1280,
      "height": 720
    }
  }
}
```

The `x` and `y` values are the top-left corner of the window's visual geometry in niri's global logical screen coordinate space.
The `width` and `height` values are logical pixels and match the window's `layout.window_size`.
This geometry describes the window visual geometry itself, not the tile: it excludes niri decorations such as borders and includes the window's offset within its tile.

`layout.global_screen_geometry` is not part of the regular window list or event-stream window layout updates.
Use `niri msg --json focused-window` or the `FocusedWindow` IPC request when you need this field.

#### Simulated Clicks

This fork adds an IPC action for scripts that need to click at a known screen position:

```sh
niri msg action simulate-click --x 720 --y 480
niri msg action simulate-click --x 720 --y 480 --button right
```

The `--x` and `--y` values are global logical screen coordinates.
They use the same coordinate space as `focused-window`'s `layout.global_screen_geometry` and output logical sizes.
For example, on a 2880x1920 physical output at scale 2, the logical output size is 1440x960, so its coordinates range from `(0, 0)` to `(1440, 960)`.

The `--button` value can be `left`, `right`, or `middle`.
If it is omitted, niri sends a left click.

The action has visible and focus-related side effects.
It warps the pointer to the requested point and leaves it there, sends normal pointer motion to the surface under that point, then sends the requested button press and release with a small delay between them.
Because the pointer motion is sent first, clients receive the expected pointer enter/motion before the button event.
The click may also activate the window under the pointer or focus an on-demand layer-shell surface, like a real click would.

The action returns an error if the coordinates are not finite, if the point is outside all outputs, if there is no Wayland surface under the point, or if the session is locked.
It does not click the desktop background, and it does not click niri's own decoration-only regions when there is no client surface at the target.

For raw JSON IPC, the request is:

```json
{"Action":{"SimulateClick":{"x":720.0,"y":480.0,"button":"Right"}}}
```

For more complex requests, you can use `socat` to find how `niri msg` formats them:

```sh
$ socat STDIO UNIX-LISTEN:temp.sock
# then, in a different terminal:
$ env NIRI_SOCKET=./temp.sock niri msg action focus-workspace 2
# then, look in the socat terminal:
{"Action":{"FocusWorkspace":{"reference":{"Index":2}}}}
```

For example, a program can ask niri to emit a finite one-off synthetic scroll gesture at the current cursor location:

```sh
niri msg action simulate-scroll --y 150
```

This action is designed for programmatic scroll injection.
It does not start the held-key keyboard scrolling state machine, does not warp the cursor, and does not use key release or decay handling.

The JSON form accepts `x` and `y` in logical points; omitted axes default to `0.0`:

```json
{"Action":{"SimulateScroll":{"y":150.0}}}
```

You can find all available requests and response types in the [niri-ipc sub-crate documentation](https://niri-wm.github.io/niri/niri_ipc/).

### Backwards Compatibility

The JSON output *should* remain stable, as in:

- existing fields and enum variants should not be renamed
- non-optional existing fields should not be removed

However, new fields and enum variants will be added, so you should handle unknown fields or variants gracefully where reasonable.

The formatted/human-readable output (i.e. without `--json` flag) is **not** considered stable.
Please prefer the JSON output for scripts, since I reserve the right to make any changes to the human-readable output.

The `niri-ipc` sub-crate (like other niri sub-crates) is *not* API-stable in terms of the Rust semver; rather, it follows the version of niri itself.
In particular, new struct fields and enum variants will be added.
