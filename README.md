# stagelanes

**Status: pre-alpha.** Nothing here is built or measured yet.

Rust runtime that reads each core's cache size, top frequency and socket, then pins pipeline stages to the cores that suit them, with lock-free batched handoff between stages. Runs on Windows and Linux.

Part of the paralanes project: `paralanes` (umbrella: spec, plan, atlas,
network join, appliance image), `bytelanes` (GPU data ops), `stagelanes`
(stage placement runtime) and `diagsteno` (strain and vibration processing).

No machine in the current test set has mixed core types, so placement by core type is untested. Placement by socket is tested on a two-socket Xeon server
(PowerEdge R440, 2x Xeon Silver 4214).

## Licence

Apache License 2.0. See [LICENSE](LICENSE).
