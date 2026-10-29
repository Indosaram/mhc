pub const TODO_FANOUT_REMINDER: &str = concat!(
    "<system-reminder>\n",
    "ultrawork mode is active and this session just started its todo list. Before working any todo:\n",
    "1. SIZE the work: weigh the todo count, each task's scope, and the total effort.\n",
    "2. COMPUTE the fan-out decision: delegate to parallel subagents only when the parallelism gain beats spawn and coordination overhead - independent parts with disjoint write scopes fan out, interdependent or trivial parts do not.\n",
    "3. TELL the user the decision either way: which parts route to which categories and why fan-out pays off, or why you are working directly. Never delegate silently and never grind through a fan-out-shaped task silently.\n",
    "4. KEEP the todo list fresh: mark start/done the instant each task transitions, append newly discovered steps the moment they surface, drop abandoned ones. A stale todo list is a defect.\n",
    "</system-reminder>"
);
