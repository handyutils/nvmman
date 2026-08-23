# nvmman

`nvmman` is a native Rust terminal UI for maintaining nvm Node.js installations
and global npm packages. It installs and verifies the latest native-platform LTS,
keeps a consolidated registry from every nvm Node version installed on the
machine, restores packages into the default Node, and offers one-by-one updates.

**Documentation:** [handyutils.github.io/nvmman](https://handyutils.github.io/nvmman/)

## Features

- Checks the official Node.js release index and installs the newest LTS.
- Detects architecture mismatches, including x64 Node on Apple Silicon.
- Scans every installed nvm Node version for global npm packages.
- Stores a durable registry at `~/.nvm/global-packages-registry.json`.
- Restores registry packages into the current nvm default without removing an
  existing package before a replacement succeeds.
- Shows package updates and asks for confirmation for every update.
- Supports keyboard navigation, mouse selection, mouse-wheel scrolling, and
  confirmation dialogs for mutating operations.

## Install

```zsh
cargo install nvmman
nvmman
nvmman update
```

`nvmman update` checks crates.io and prints the exact installation command when
a newer release is available.

## Shortcuts

| Key | Action |
| --- | --- |
| `1`-`5` | Switch dashboard, packages, registry, updates, and activity views |
| `r` | Refresh machine state |
| `l` | Install latest LTS and make it default |
| `g` | Sync the global package registry |
| `a` | Restore registry packages into the default Node |
| `u` | Check packages for updates |
| `Enter` | Activate the selected action or update |
| `j` / `k`, arrows, mouse wheel | Navigate and scroll |
| `q` / `Esc` | Quit or close a dialog |
