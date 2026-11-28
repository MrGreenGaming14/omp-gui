# C++ rewrite execution runbook

How `CPP_PORT:` issues are claimed, worked and merged into `cpp-rewrite`. Porting rules are in [RECIPE.md](RECIPE.md).

1. **Prerequisites.** Clone the repo and check out `cpp-rewrite`. Run the F1 apt line and install rustup. Authenticate `gh` as `Sweet-Rice`. Clone the repo to `~/omp-gui-cpp` and use it as the integration worktree.
2. **Scheduler.** An eval Python cell with a fixed set of `agent()` handles, **at most 5 live**.
   - Ready set: open `cpp-port` issues whose `## Depends on` issues are all closed. Find them with `gh issue list --label cpp-port --state open --json number,title,body,assignees` and `gh issue view` for each dependency.
   - Skip issues that already have an assignee.
   - Start ready issues in ascending issue number.
3. **Claim issue `N`, before spawning its agent:**
   - `gh issue edit N --add-assignee Sweet-Rice --add-label "In Progress"`
   - `gh issue comment N --body "Claimed: working on branch cpp-port/issue-N-<slug>."`
   - `git -C ~/omp-gui-cpp worktree add ~/omp-gui-cpp-N -b cpp-port/issue-N-<slug> cpp-rewrite`
4. **Issue agent.** Use `agent()` (default `task` agent) with label `issue-N`. The prompt contains:
   - the issue body;
   - `docs/cpp-port/RECIPE.md`;
   - the worktree path;
   - this rule: "Split into file-disjoint sub-slices with the `task` tool when that helps (for example parser vs. transport vs. tests); you own integration. Run only your own targets. Commit on your branch and reply with the SHA and the checks you ran."
5. **When an agent finishes:**
   1. Merge: `git -C ~/omp-gui-cpp merge --no-ff cpp-port/issue-N-<slug>`. On conflicts, resolve mechanical ones directly. Otherwise spawn an agent on the branch with the conflict text.
   2. Gate: `cmake --build build` in `~/omp-gui-cpp`.
   3. Push: `git push origin cpp-rewrite`.
   4. Close: `gh issue comment N --body "Done in <sha> on cpp-rewrite. Checks: <list>"`, `gh issue edit N --remove-label "In Progress"`, `gh issue close N`.
   5. Tick the issue on the tracking issue, remove the issue worktree, then refill from the ready set.
6. **On failure** of the agent or the gate:
   - Comment the failure on the issue and retry once with a new agent given the failure text.
   - If the retry also fails: `gh issue edit N --remove-assignee Sweet-Rice --remove-label "In Progress"`, comment `Blocked: <reason>`, and continue. Issues that depend on it stay blocked.
7. **If builds run out of memory:** lower the cap to 3 live issues and set `CMAKE_BUILD_PARALLEL_LEVEL=4`.
