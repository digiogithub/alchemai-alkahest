---
id: ALK-US-0108
type: story
title: "Desktop dialogs: Esc and backdrop click cancel; retry countdown in connection-lost banner"
status: in_progress
parent: ALK-EP-0013
author: mcp
labels: [desktop]
created: 2026-10-17T05:56:00Z
updated: 2026-10-17T06:43:09Z
started: 2026-10-17T06:43:09Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-17T05:56:00Z
---

Found while implementing ALK-US-0081 / ALK-T-0219 (screen 17).

- Screen 17.9-17.14: "Esc and the backdrop cancel" and Cancel has default focus. The shared `views::widgets::dialog` closes only through its buttons today; needs a focus handle on the dialog and a backdrop click handler that does not fire for clicks inside the card.
- Screen 17.5: the connection-lost banner should show "Retrying in N s · attempt K · last seen HH:MM:SS". `connection::Snapshot` does not carry the backoff delay, attempt count or last-seen time; add them to the snapshot (the machine knows them) and tick the banner.
- Screen 17.8: "Install prebuilt instead" on a failed source build and "Unload others and retry" / "Lower context" on a load failure are not wired.
