#!/usr/bin/env python3
"""Regenerate the pw-dump fixtures and Python reference output (read-only).

Usage: make_reference.py RAW_DUMP.json
  RAW_DUMP: `pw-dump --no-colors` output (read-only capture)
Writes monitor.json (the cleaned snapshot as its first batch, followed by synthetic
incremental monitor batches in pw-dump's format) and python_reference.json.
Only Node/Device/Link/Metadata objects are kept and host/user identifying
properties are dropped. The reference is computed with the Python
audio_backend.parse_snapshot / merge_update / split_batches (source of truth).
"""
import json, sys
from dataclasses import asdict
from pathlib import Path

sys.path.insert(0, str(Path.home() / "tessel-bluetooth/tessel-audio"))
import audio_backend as backend  # noqa: E402

HERE = Path(__file__).resolve().parent
KEEP = {"Node", "Device", "Link", "Metadata"}
DROP = ("application.process.", "host", "user", "object.path", "api.bluez5.address",
        "device.serial", "api.alsa.card.longname")


def clean(objects):
    out = []
    for obj in objects:
        if obj.get("type") is None and obj.get("info", 0) is None:
            out.append(obj)  # removal
            continue
        if backend._kind(obj) not in KEEP:
            continue
        props = (obj.get("info") or {}).get("props")
        if isinstance(props, dict):
            for key in list(props):
                if any(part in key for part in DROP):
                    props.pop(key)
        out.append(obj)
    return out


def synthetic(snapshot):
    """Incremental batches shaped like `pw-dump --monitor` updates."""
    nodes = [o for o in snapshot if backend._kind(o) == "Node"]
    sink = next(o for o in nodes if o["info"]["props"].get("media.class") == "Audio/Sink")
    other = next(o for o in nodes if o is not sink)
    device = next(o for o in snapshot if backend._kind(o) == "Device")
    return [
        [{"id": sink["id"], "type": sink["type"], "info": {"change-mask": ["params"],
          "params": {"Props": [{"mute": True, "channelVolumes": [0.125, 0.064]}]}}}],
        [],
        [{"id": other["id"], "info": None},
         {"id": device["id"], "type": device["type"], "info": {"change-mask": ["props", "params"],
          "props": dict(device["info"]["props"], **{"device.description": "Renamed card"}),
          "params": {"Profile": [{"index": 0}]}}},
         {"id": 9999, "type": "PipeWire:Interface:Port", "info": {}},
         {"id": 9998, "type": "PipeWire:Interface:Node", "info": {"state": "running", "props": {
             "object.serial": 99980, "media.class": "Stream/Output/Audio", "node.name": "synthetic",
             "application.name": "Synthetic", "target.object": str(sink["info"]["props"].get("object.serial"))},
             "params": {"Props": [{"volume": 1.0, "mute": False, "channelVolumes": [1.0, 0.0],
                                   "channelMap": ["FL"]}]}}}],
        [{"id": sink["id"], "type": sink["type"], "info": {"change-mask": ["state"], "state": "suspended"}}],
    ]


def model(nodes, devices, defaults):
    return {"nodes": [asdict(n) for n in nodes.values()],
            "devices": [asdict(d) for d in devices.values()],
            "defaults": defaults}


def main():
    snapshot = clean(json.load(open(sys.argv[1])))
    batches = [snapshot] + synthetic(snapshot)
    stream = "".join((json.dumps(b, indent=2) if b else "[\n]") + "\n" for b in batches)
    (HERE / "monitor.json").write_text(stream)
    reference = {"snapshot": model(*backend.parse_snapshot(snapshot)), "monitor": []}
    graph = {}
    split, rest = backend.split_batches(stream.encode())
    assert not rest
    for batch in split:
        changed, props_only = backend.merge_update(graph, json.loads(batch))
        reference["monitor"].append({"changed": sorted(changed), "props_only": sorted(props_only),
                                     "model": model(*backend.parse_snapshot(list(graph.values())))})
    (HERE / "python_reference.json").write_text(json.dumps(reference, indent=1) + "\n")


main()
