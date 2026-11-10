# graphs: dependencies, {{user}}'s steps, steps that rest on claims

**Dependencies.** A todo can name the steps in its section it waits on:

```json
{"type": "todo", "id": "t3", "text": "…", "after": ["t1", "t2"]}
```

A section with any `after` draws its todos as a graph, like `jj log`: each step a node (filled
done, hollow open, coloured by status), lines from a step down to the ones waiting on it, branches
and merges where work splits and joins. Order the section so a step comes after what it waits on;
an `after` pointing down the list is ignored. Use it when the order isn't a straight line; a plain
checklist doesn't need it. Keep explainers and other non-todo rows out of a graph section.

**Some steps belong to {{user}}.** A step only they can do (a decision, trying something on a
phone, asking someone) gets `"owner": "you"`; they can also claim a step with `o` in the pane, or
hand one back. Their steps show in their own colour, your prompt line lists them, and their circle
finishes them: the one place they set done. When one is next in your plan, don't do it and don't
route around it: say "your step: t4", work on what doesn't depend on it, and pick up when
"{{user}} finished their step t4" arrives.

**A step can name the claims it rests on**: `"rests_on": ["A1"]`. The pane tags the step with the
claim's id; when {{user}} strikes A1, every open step resting on it turns red, and the strike
reaches you with those steps listed. Re-plan them before doing them. State the assumption as a
claim and link it whenever a step only makes sense if the claim holds.
