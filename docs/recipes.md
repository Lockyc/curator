---
type: guide
description: Config recipes for common curator setups — multiple accounts, a notification hub, an operator console, self-signed hosts.
links:
  - rel: part-of
    to: README.md
---

# Recipes

curator's config makes a few setups easy. All four combine freely — one window can mix them. Every
key is in the README's [Configuration](../README.md#configuration) tables.

## Same web app, multiple accounts side by side

Point several tabs at the *same* `url` and give each a distinct `session`. curator keeps the
logins fully isolated, so you get parallel accounts of one web app — Matrix/Element
homeservers, Slack workspaces, separate Google logins — with no incognito juggling. Mark them
`load_on_open` and every account stays live and notifies at once.

```toml
[[window]]
title = "Matrix"

  [[window.group]]
  name = "Accounts"

    [[window.group.tab]]
    title        = "Work"
    url          = "https://app.element.io/"
    session      = "element-work"
    load_on_open = true

    [[window.group.tab]]
    title        = "Personal"
    url          = "https://app.element.io/"
    session      = "element-personal"
    load_on_open = true
```

## A background notification hub

Keep alert and status pages `load_on_open` so they stay live, fire native banners on new
activity even when curator isn't focused, and roll their unread up to the dock badge. Leave
everything else lazy so only the pages you *want* live cost you a banner.

```toml
  [[window.group]]
  name = "Alerts"

    [[window.group.tab]]
    title        = "ntfy"
    url          = "https://ntfy.example.com/alerts"
    load_on_open = true

    [[window.group.tab]]
    title        = "Status"
    url          = "https://status.example.com/"
    load_on_open = true
```

## An operator console for self-hosted infra

Collect the sprawl of admin UIs — hypervisor, NAS, DNS, reverse proxy, registrar — into named
groups in one window. Give the window a `colour` so it's instantly recognisable, and let most
tabs stay lazy and quiet; only the dashboards you watch get `load_on_open`.

```toml
[[window]]
title  = "Infra"
colour = "#0f8a8a"

  [[window.group]]
  name = "Network"

    [[window.group.tab]]
    title = "UniFi"
    url   = "https://unifi.example.com/"

    [[window.group.tab]]
    title = "Proxmox"
    url   = "https://pve.example.com/"

  [[window.group]]
  name = "DNS & Domains"

    [[window.group.tab]]
    title = "Cloudflare"
    url   = "https://dash.cloudflare.com/"
```

## Homelab devices with self-signed certs

List the hosts whose invalid/self-signed TLS you trust in the app-global `allow_insecure` so
their admin pages load without the browser security wall. Scope it tightly — only the hosts
you actually run.

```toml
allow_insecure = ["10.0.0.1", "nas.local"]
```
