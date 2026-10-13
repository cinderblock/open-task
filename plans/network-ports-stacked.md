# Network: fold virtual switch ports into their network card, stacked

> **Status:** done (`05ce365`, `58de653`) · **Started:** 2026-10-13 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)
> Related: `plans/performance-view.md` (the page's design; its finding on which
> interfaces count as connections).

## Goal

The user's machine has one physical network card and many VLANs, each a Hyper-V
`vEthernet` port on an external virtual switch bound to that card. The app lists
every port as a peer of the card, which hides how they relate and double-counts:
the card's traffic *is* the ports' traffic. Show the ports as parts of the card:
fold them into it in the device list, and on the card's page stack each port's
traffic as a band of the card's total, the way the History chart stacks
programs. Each port keeps its own page.

## Environment / context

- This machine: `Ethernet` (Intel I219-V, physical) → external switch
  `VLAN-vSwitch` → host vNICs `vEthernet (VLAN-vSwitch)` (untagged, shares the
  card's MAC), `VLAN-3`, `VLAN-5`, `VLAN-7`, `VLAN-11`, `VLAN-12`, `VLAN-99`,
  `VLAN-FMS` (VLAN 4). Also two internal switches with no uplink:
  `vEthernet (Default Switch)`, `vEthernet (WSL (Hyper-V firewall))`; and Tailscale.
- Probe: `crates/ot-probe/src/imp/windows/network.rs` (discovery every 5 s).
- Model: `crates/ot-model/src/device.rs` `AdapterInfo`.
- Recorder: `crates/ot-record` (postcard tables of `AdapterInfo`; `format.rs`
  `FORMAT_VERSION`).
- UI: `crates/ot-ui/src/perf.rs` (Performance page), `pages/summary.rs`
  (Network card), `charts.rs` (`ChartGroup`, shared hover), `sparkline.rs` (`Plot`).
- Console print: `crates/ot-app/src/main.rs` (`--print`-style adapter lines).

## Decisions already made (don't re-ask)

1. **List: folded into the card.** Only the card is listed; its page shows the
   breakdown, and a click on a legend row opens that port's page (user's choice
   over "nested, always shown" and "flat").
2. **Card page: two stacked charts**, Receive and Send, one band per port plus a
   grey "Other" band (card total minus the ports: VMs' traffic, broadcasts). A
   shared legend; hover reads out each band. A **Stack by port / Total** switch in
   the caption flattens back to today's two-line chart (the user's "flatten"
   checkbox). Not persisted, like the CPU's Overall / Logical processors switch.
3. **Parentage comes from the interface stack** (`GetIfStackTable`, no elevation):
   walk down from each listed interface, through filters and the switch's internal
   adapter, to the nearest *listed* interface below. Needs no Hyper-V WMI.
4. A port of an *internal* switch (Default Switch, WSL) has no listed interface
   below it and stays top-level, as it should: its traffic never touches a card.

## Plan / steps

1. ~~Probe + model: `AdapterInfo::parent: Option<u64>`; stack-table walk in
   discovery; natural name order (`VLAN-3` before `VLAN-11`).~~ `05ce365`
2. ~~Recorder: format version 4; read 1–3 through `AdapterInfoV3`.~~ `05ce365`
3. ~~Performance page: fold ports into the card's entry; stacked charts + legend
   + Total / By port switch; port page names its card.~~ `58de653`
4. ~~Summary card and console print: ports under their card.~~ `05ce365`, `58de653`
5. ~~README; screenshot check.~~ Done (dark and light, card and port panes,
   Summary).

## Findings / gotchas

- **The stack table has the chain** (this machine, `GetIfStackTable`, higher →
  lower): `vEthernet (VLAN-11)` (3) → 40 (unnamed filter) → `vSwitch (VLAN-vSwitch)`
  (36) → 51 (filter) → `Ethernet` (13). Default Switch: `vEthernet (Default Switch)`
  (56) → 38 → `vSwitch (Default Switch)` (29) → nothing.
- **The card's counters include the ports'.** Over 10 s: `Ethernet` rx 966.7 KB/s,
  tx 55.9; sum of the `VLAN-*` ports rx 963.0, tx 52.8. So stacking the ports
  inside the card's total is honest, and "Other" is the small remainder. A port
  talking to a VM on the same switch would count on the port but not the card, so
  the sum can exceed the card: clamp "Other" at zero, scale to the taller.
- All adapter series are pushed at the same tick, so their buckets and a chart's
  columns line up across ports; the stack can sum plots column by column.
- Adding a field to `AdapterInfo` changes the recorder's table layout (postcard
  writes fields in order, records are `Vec<(id, value)>`): needs a format bump.
- The stacked charts are not `ChartGroup` charts: the card's slot sits in the
  group only so the shared hover snaps to it; the bands are drawn from `Plot`s the
  page keeps (`Stack`), in layers `STACK_LAYERS` that `repaint_chart` rebuilds on
  clock-only frames. Easing of a new sample (`TimeAxis::settle`, added alongside by
  the chart-live-edge thread) happens in `Plot::build`, so ports and total ease
  together.
- Live on this machine nearly all traffic is the untagged `VLAN-vSwitch` port; the
  tagged VLANs are a few Kbps. "Other" was ~22 Kbps against ~4.5 Mbps.
- Known limits, not worked on: more than eight ports share the grey of "Other"
  (the theme has eight series colors); the VLAN *id* is not shown (only Hyper-V
  WMI has it; names usually carry it).

## Progress log

- [x] Topology and counters checked on this machine; design chosen
- [x] Probe + model (`05ce365`)
- [x] Recorder v4 (`05ce365`)
- [x] Performance page (`58de653`)
- [x] Summary + console (`05ce365`, `58de653`)
- [x] README, screenshots, commits

## Open questions for the user

(none)

## Things not to do

- Don't use Hyper-V WMI (`Msvm_*`) for the mapping: needs Hyper-V Administrators
  and is slow; the stack table already has it.
- Don't change network settings on this machine to test (see the
  `hyperv-tagged-vnic` skill); read-only queries only.
- Don't `sed -i` source files here: Git Bash's sed writes LF and the repo is CRLF.
