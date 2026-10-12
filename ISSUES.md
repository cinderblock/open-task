# Issues

Problems noticed while working on something else, left for a thread of their own.

## The History view keeps the GPU busy

in progress — master, GPU-breakdown thread (`plans/history-gpu-cost.md`)

Seen 2026-10-12 on an Intel UHD (integrated) machine, debug build of `a6166dc`
plus the GPU History work: with the window on the History view (the default CPU
reading, nothing touched), open-task's share of the GPU's 3D engine read 30.3,
46.5, 48.9 and 33.8 % over four one-second samples of
`\GPU Engine(pid_<open-task>_*engtype_3d)\Utilization Percentage`. On the List
view the same samples read 4.7 to 6.9 %. The History's GPU Percent reading shows
the same thing: open-task.exe is the biggest band, about half the GPU.

Reproduce: `target\debug\open-task.exe --view history`, then watch the counter
above (or the History chart's GPU / Percent reading). Matters because an idle
task manager shouldn't take half an integrated GPU; on a laptop that's battery.
