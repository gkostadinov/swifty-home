# {{name}} for Swifty Home

A Swifty Home plugin for {{name}} devices.

> Written by `swifty plugin new`. Replace each part marked TODO with what is true for
> your plugin before you open a pull request.

## What it supports

| Kind | What it can do |
| --- | --- |
{{class_rows}}

TODO: name the models you have, and what they do not support.

## Setup

In the app: *Integrations → Add → {{name}}*. In a terminal:

```
sudo swifty plugin add {{id}}
```

{{#local}}
You need each device's IP address. Give the devices fixed addresses in your router,
so they do not move.
{{/local}}
{{#cloud}}
You need an access token from the vendor. TODO: say where to get one.
{{/cloud}}

## What it reaches

{{#local}}
- UDP to each device on the local network, port {{port}} unless its row says otherwise.
- Nothing outside your network.
{{/local}}
{{#cloud}}
- HTTPS to the vendor's API, `api.example.org` unless the config says otherwise.
- The vendor's servers reach the devices; this plugin never talks to them directly.
{{/cloud}}

It reaches nothing while the hub is in dry-run.

## Tested on hardware

TODO: the models and firmware versions you tested with, and when.
