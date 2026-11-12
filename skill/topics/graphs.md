# graphs: {{user}}'s steps, steps that rest on claims, lanes between steps

**Some steps belong to {{user}}.** A step only they can do (a decision, trying something on a
phone, asking someone) gets `"owner": "you"`; they can also claim a step with `o` in the pane, or
hand one back. Their steps show in their own color, your prompt line lists them, and their circle
finishes them: the one place they set done. When one is next in your plan, don't do it and don't
route around it: say "your step: t4", work on what doesn't depend on it, and pick up when
"{{user}} finished their step t4" arrives.

**A step can name the claims it rests on**: `"rests_on": ["A1"]`. The pane tags the step with the
claim's id; when {{user}} strikes A1, every open step resting on it turns red, and the strike
reaches you with those steps listed. Re-plan them before doing them. State the assumption as a
claim and link it whenever a step only makes sense if the claim holds.

**Lanes.** `"after": ["t1", "t2"]` on a todo names the steps above it in its section that it waits on; the section draws them joined by lanes.
A table draws the same lanes between its rows: a column of kind `graph` and `after` on a `tr` (`margin help dashboards`, "A graph column").
