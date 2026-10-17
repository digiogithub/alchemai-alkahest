---
id: ALK-T-0215
type: task
title: Generate platform icon sets from design/brand
status: done
priority: high
parent: ALK-US-0064
milestone: ALK-M-0005
labels: [desktop]
effort: 2
created: 2026-10-16T22:52:18Z
updated: 2026-10-17T02:33:48Z
closed: 2026-10-17T02:33:48Z
---

## Description

`cargo xtask icons`: render `design/brand/app-icon.svg` to PNG sizes (16–1024), `.icns` and `.ico`; small sizes use `mark-small.svg`, tray uses `mark-tray.svg`. Output under `packaging/icons/`.

Part of ALK-US-0064.
