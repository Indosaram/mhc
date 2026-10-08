# Real-surface prerequisites

The packaging OMO checkout is /home/indo/code/oh-my-openagent at 77f3067f157a4f88e6d8ed48b3a6c338654402ed, matching PINS.md. rsvg-convert and the residual driver's xterm-headless dependency are available. The existing deterministic loopback library, PTY driver, and PNG renderer were restored into the integration worktree's ignored evidence harness using apply_patch; no historical result files were copied.

script/qa/web-terminal-visual-qa.mjs is absent in both the integration worktree and original project tree, and a wider lookup found no copy in the installed OMO package. The objective's conditional invocation is therefore unavailable. The residual driver provides fresh raw ANSI, xterm cell grids, and explicitly derived PNGs; these PNGs must not be described as literal browser screenshots. Visual inspection and cleanup evidence are still required.

The preinstall tui scenario is an early real-surface probe against the current freshly built mhc. It does not replace the required installed scenario-all gate after workspace binary build and package installation.
