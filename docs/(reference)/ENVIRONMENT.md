---
title: Environment variables
description: Pix environment variables that are part of the command-line contract.
---

These variables are intentional CLI inputs. Other variables used by CI, the
relay deployment, tests, or Pi itself are not Pix configuration and are not
listed here.

| Variable | Purpose | Format and default | Applies to |
| --- | --- | --- | --- |
| `PIX_CONFIG` | Select the Pix configuration file | A file path. Unset uses `$HOME/.config/pix/config.json`. | Every CLI command |
| `PIX_OUTPUT` | Select output format | `human` (default) or `json` | Every CLI command |
| `PIX_RELAY_URL` | Supply the relay endpoint to setup | A valid `ws://` or `wss://` URL without credentials or a fragment. Unset leaves setup to its normal prompt/default. | `pix setup` |
| `PIX_WORKSPACE` | Supply the workspace root to setup | A path to an existing directory. Unset leaves setup to its normal prompt. | `pix setup` |

Examples:

```sh
PIX_CONFIG="$HOME/.config/pix-headless/config.json" pix status
PIX_OUTPUT=json pix --no-input status
PIX_RELAY_URL=wss://relay.example.com \
  PIX_WORKSPACE="$HOME/Projects/app" \
  pix setup --non-interactive --no-pair
```

`PIX_OUTPUT=json` selects the same versioned envelope as
`pix --output json`; JSON mode never opens an interactive menu. `PIX_CONFIG`
also lets the optional Pi TUI extension find the matching host-local bridge
when Pi runs with that configuration.

For settings persisted by these commands, see [Configuration](/docs/configuration).
