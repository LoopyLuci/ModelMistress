# Running in a ComputeFabric

ComputeFabric (CF) is a fabric of paired nodes: it places jobs and services, moves files between machines and keeps their
state. ModelMistress is the model hub on each machine. Together they serve one set of models from every machine you
own. The design is in CF's `docs/MODELMISTRESS-INTEGRATION.md`; this page is the hub's side.

The hub does not need CF, and CF does not need the hub. Nothing here is on until you set it up.

## What each side does

| | ModelMistress | ComputeFabric |
|---|---|---|
| Finds, loads, serves, meters models on one machine | yes | asks the hub; never loads a file itself |
| Knows the other machines | no | yes (paired nodes, their route and round trip) |
| Reports free VRAM and RAM | `machine.status` | puts it in the node's heartbeat |
| One OpenAI endpoint for all machines | no | the endpoint on `127.0.0.1:7880`, which picks a node per request |
| Passes a request it cannot serve to another machine | `fabric_offload` (below) | the endpoint takes it |

## Join a fabric

On each machine, start CF with `[models] enabled = true` in its config (CF's `docs/DESIGN.md`, the models section). CF finds
the hub through its `control.json` (set `models.home` or `MM_HOME`), and reports what the hub has loaded and installed
to the other nodes. Nothing changes in the hub for this: CF uses the owner's token on the same machine, through the
operations `service.status`, `model.list`, `machine.status`, `model.plan`, `model.load`, `model.unload` and `usage.get`.

To let the hub pass work on, tell it where CF is:

```bash
model-mistress call fabric.set '{"api":"http://127.0.0.1:7878","endpoint":"http://127.0.0.1:7880","token_file":"<file with the output of cf models token>"}'
model-mistress call fabric.status
```

This writes `<home>/fabric.json`. `fabric.set` with `{"leave": true}` removes it. The token file holds the endpoint's bearer
token (`cf models token`); keep it out of any folder that is committed or shared.

## Offload

With the settings above and `offload` left at its default, `when_full`, a request that fails because this hub has no room for
the model (`insufficient_memory`, `insufficient_vram`) or has no free slot (`busy`, with `fail_fast_busy` on) goes to CF's
endpoint instead. CF sends it to the node where the model is loaded, or fits best, and the answer comes back through this
hub. The reply carries `x-mm-offloaded: 1` and `x-mm-node: <node>`. If no node can serve it either, the hub's own error is
returned.

- A request carries `x-cf-hop`. A hub that receives a request with a hop does not offload it again, so a request cannot
  go round the fabric.
- `offload: "off"` keeps every request on this machine.
- Streams are passed through as they arrive.

Usage: the caller's key is charged for an offloaded request as for one served here (requests, tokens, its quotas), from the
`usage` in the fabric's answer. `fabric.status` shows `offloaded_by_node`, a count per serving node since the hub started.
CF's `models.usage` has the full figures by caller, model and node.

## Operations

| Operation | Does |
|---|---|
| `machine.status` | each GPU's free memory, what models may use of it (`vram_usable_mib`), RAM, the Windows commit, what is loaded and running |
| `fabric.status` | whether the hub is joined, the offload mode, whether CF answers |
| `fabric.nodes` | every node with a hub, what each has loaded, free VRAM and RAM (asks CF) |
| `fabric.models` | every model in the fabric and where it is |
| `fabric.route` | which node would serve a model now, and why |
| `fabric.set` | join, change or leave |

## Two ways onto the network, which do not conflict

`overlay/` (the `mm-overlay` crate) publishes a hub to *paired devices* with a peer key: a phone uses the desktop's models.
CF's model gate publishes a hub to *paired fabric nodes* through CF's own overlay node and an allow-list of operations,
using the owner's token on the hub's own machine, so no key leaves it. Use the first for phones and the second for the
machines of one fabric.
