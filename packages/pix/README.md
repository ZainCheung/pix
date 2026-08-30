# Pix

Pi extension for the Pix host-local TUI bridge.

## Install

```sh
pi install npm:@zaincheung/pix
```

Restart Pi, or run `/reload` in an existing Pi session, after installing.
Pix must already be installed and its host service must be running. The
extension is optional: when Pix is unavailable, Pi continues normally and no
Pix status is shown in the footer.

If an older Pix release installed a copy at
`~/.pi/agent/extensions/pix-bridge`, remove that legacy copy before enabling
this package. Pi loads both locations when both are present.

## Development

This package contains the complete bridge extension in `index.ts`. The host
side of the protocol remains in the Pix Rust workspace; the extension only
connects to the host-local Unix socket and never replaces the `pi` executable.
