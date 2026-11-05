# file-browser-tui

<!-- TODO: add a screenshot as demo.png -->
<!-- ![file-browser-tui screenshot](demo.png) -->

A Dolphin-like file manager for the terminal, built with [ratatui](https://ratatui.rs).
It aims for Dolphin's look and habits (places sidebar, icon grid, breadcrumbs,
right-click menus, Dolphin's shortcuts) and reads the same desktop files Dolphin does:
your icon theme, the shared thumbnail cache, folder icons from `.directory` files and
KDE service menus. In [kitty](https://sw.kovidgoyal.net/kitty/) it shows real theme
icons, thumbnails and smooth translucent menus; other terminals get text-drawn icons.

## Features

- **Three views**: Icons (zoomable, Dolphin's grid), Compact and Details (sortable
  columns, expandable folder tree). View settings are remembered per folder.
- **Show in Groups**: by name, date ("Today", "Yesterday", months, ...), size or type,
  in Details and Icons view, per folder.
- **Thumbnails** for images, videos (with film-strip edges), audio cover art, EPUB,
  comic books, OpenDocument and PDF, plus Dolphin-style folder previews. They go in the
  freedesktop cache (`~/.cache/thumbnails`), so Dolphin and the app share them.
  Thumbnails on screen come first, then the screens around them, then the first items
  of the folders you can see, so scrolling and opening folders feel instant.
- **Places sidebar**: drag to reorder, right-click to add or remove; devices with usage
  bars and an eject button for removable drives and FUSE mounts; KDE Connect phones.
- **Location bar**: clickable breadcrumbs, or type a path with completion
  (`Ctrl+L`, `F6`, `Alt+D`).
- **Search** by file name (`fd`) or inside files (`rg`), in the current folder or below.
- **File operations** done by the app itself: copy, move, trash (freedesktop trash
  spec) and delete, with a progress bar, time left and cancel, and Dolphin's
  "File Already Exists" choices (Overwrite / Write Into, Skip, Rename, Apply to All).
- **Clipboard** that works with other file managers: cut/copy/paste files between the
  app and Dolphin, Nautilus etc., and paste copied text or images as a new file.
- **Drag and drop**: rubber-band selection, dragging items onto folders and places, and
  (in kitty) dropping files in from other programs.
- **Right-click menus**: Open With (your desktop's apps), Open Terminal Here, Compress,
  Send to Device (KDE Connect), Properties, and your own actions. Existing KDE service
  menus are imported on first run.
- **Configurable** keyboard shortcuts and colors.

## Requirements

- Linux
- [kitty](https://sw.kovidgoyal.net/kitty/) for icons, thumbnails, smooth menus and
  drag and drop. Other terminals work, with text-drawn icons and no images.
- A [Nerd Font](https://www.nerdfonts.com/) for menu and toolbar glyphs.
- An icon theme (whatever your desktop uses, e.g. Breeze or Papirus) and `rsvg-convert`
  (librsvg) to draw its icons.

Optional tools, used when present (run `file-browser-tui --check` to see what's found):

| Tool | For |
| --- | --- |
| `wl-clipboard` (Wayland) or `xclip` (X11) | the clipboard |
| `fd`, `ripgrep` | searching by name, searching inside files |
| `ffmpeg` (`ffmpegthumbnailer` if you prefer it) | video thumbnails, audio cover art |
| `poppler` (`pdftoppm`) | PDF thumbnails (enable `gsthumbnail`, see below) |
| `xdg-utils` | opening files with their default app |
| `zip` | Compress → zip |
| `kdeconnect` | Send to Device, browsing phones |
| `udisks2` | unmounting and safely removing drives |
| `glib2` (`gio`) | file type icons for unusual files |

## Install

### Arch Linux (AUR)

```sh
yay -S file-browser-tui
```

### Build from source

```sh
cargo build --release
./target/release/file-browser-tui
```

## Running it

```sh
file-browser-tui [PATH]          # in the current terminal
file-browser-tui --kitty [PATH]  # in a new kitty window set up for the app
```

`PATH` is a folder to open, or a file to open its folder with it selected.

Use `--kitty` in launchers, desktop entries and window manager bindings. It opens a
kitty window without padding, so images line up with the edges, and lets Shift+click
and Ctrl+Shift+click reach the app instead of kitty's own selection. The included
`file-browser-tui.desktop` does this.

## Keyboard shortcuts

Dolphin's defaults; `F1` in the app lists them, including any you've changed.

| Key | Action |
| --- | --- |
| `Enter` | open |
| `Backspace`, `Alt+←` | back |
| `Alt+→` | forward |
| `Alt+↑` | up a folder |
| `Ctrl+L`, `F6`, `Alt+D` | edit the location |
| `Ctrl+F` | search |
| `Ctrl+A` | select all |
| `Shift+arrows`, `Shift/Ctrl+click` | extend the selection |
| `Ctrl+C` / `Ctrl+X` / `Ctrl+V` | copy / cut / paste |
| `Ctrl+Alt+C` | copy the location |
| `F2` | rename |
| `F10` | new folder |
| `Delete` / `Shift+Delete` | move to trash / delete permanently |
| `Alt+Enter` | properties |
| `Shift+F4` | open a terminal here |
| `Ctrl+H`, `Alt+.` | show or hide hidden files |
| `F5` | reload |
| `Ctrl+1` / `Ctrl+2` / `Ctrl+3` | Icons / Compact / Details view |
| `Ctrl+Plus` / `Ctrl+Minus` | larger / smaller icons |
| `Alt+[` / `Alt+]` | narrower / wider sidebar |
| `Ctrl+Q` | quit |

## Configuration

`~/.config/file-browser-tui/config.toml` is created on first run with every option
listed and commented out. View settings in it are the defaults for folders you haven't
changed; changes you make in a folder are remembered for that folder.

### Keys

List an action to replace its default keys; an empty list unbinds it.

```toml
[keys]
new_folder = ["F10", "Alt+F"]
rename = ["F2", "Alt+R"]
up = ["Alt+Up", "Shift+Backspace"]
```

### Colors

The default look is [Nord](https://www.nordtheme.com/). Set any of the roles listed in
the generated config; anything left out keeps its default.

```toml
[theme]
accent = "#88c0d0"
selection = "#3b4252"
```

### Thumbnails

Which files get thumbnails follows Dolphin's preview settings (`dolphinrc`) if you have
them, otherwise Dolphin's defaults. Override it with Dolphin's plugin names:

```toml
preview_plugins = ["imagethumbnail", "jpegthumbnail", "svgthumbnail", "ffmpegthumbs",
    "audiothumbnail", "ebookthumbnail", "opendocumentthumbnail", "comicbookthumbnail",
    "gsthumbnail"]
```

To keep a folder's thumbnails off disk (made when shown, never cached):

```toml
thumbnail_cache_exclude = ["~/Private"]
```

### Right-click actions

Each `*.toml` file in `~/.config/file-browser-tui/actions/` adds actions to the
right-click menu (a `README.md` there explains the format). For example:

```toml
submenu = "Convert"
mime = ["video/*"]

[[action]]
label = "Convert to webm"
icon = "video"
command = "ffmpeg -i %f -y %f.webm"
```

## License

[GNU General Public License v3.0](LICENSE)
