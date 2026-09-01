---
title: Workspaces
description: Choose which local folders Pix is allowed to expose to a paired device.
---

A workspace is a local folder that you explicitly authorize for Pix. Pi uses
that folder for its repository and tools; Pix does not browse arbitrary paths.

## Add a workspace

During `pix setup`, choose the folder Pi should use. You can add another folder
later from the host:

```sh
pix workspace add /absolute/path/to/project
pix workspace list
```

The command records the folder as an authorized workspace. A name can be
provided with `--name` if you manage several projects.

## Switch workspaces

In Pix, choose a workspace before opening its sessions. Each workspace has its
own Pi session list, so selecting another workspace changes which local
sessions the phone can open.

## Remove a workspace

List the authorized folders, note the workspace ID, and remove its
authorization:

```sh
pix workspace remove <workspace-id>
```

Removing a workspace changes Pix access only. It does not delete the folder,
its files, or Pi session data. A running host releases sessions that are no
longer inside an authorized workspace when it refreshes its configuration.

## If a workspace disappears

The folder must still exist and resolve to the same local directory. If it was
moved or replaced, remove the old entry and add the current folder again. See
[Troubleshooting](/docs/troubleshooting) for the checks.

## Next

- [Sessions](/docs/sessions)
- [Security and privacy](/docs/security)
- [CLI reference](/docs/cli)
