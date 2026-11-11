# diagrams: the house style for a diagram typed by hand: boxes, edges, arrowheads, width

No component draws a diagram yet. This is the style for any diagram you type by hand (in a fence
of a `margin doc`, in chat, in a commit message), so that every one reads the same way.

- **Prefer a tree, a numbered flow or a table** when the shape allows; draw boxes only when the
  thing has edges a list cannot show.
- **At most 64 cells wide.** It then fits a split pane without wrapping.
- **Boxes have rounded light corners** (`╭ ╮ ╰ ╯`, sides `│`, top and bottom `─`), with one space
  inside each side: `│ handler │`.
- **An edge leaves a box through a joint** in its border (`├` on a side, `┬` on the bottom), **and
  its head touches the target's border**: no gap between the arrowhead and the box it points at.
- **Labels**: above a horizontal edge, or breaking it (`── rows ──`); one space to the right of a
  vertical edge.
- **Arrowheads are the Nerd Font carets**: U+F0DA right, U+F0D9 left, U+F0D8 up, U+F0D7 down.
  Where no Nerd Font is present use `► ◄ ▲ ▼`.
- **Pad every line to the same width** with spaces. No tabs, no emoji.

A pipeline: three boxes, one label above its edge and one breaking it.

```text
           emit                                  
╭──────╮          ╭─────────╮           ╭───────╮
│ cron ├─────────│ handler ├── rows ──│ board │
╰──────╯          ╰─────────╯           ╰───────╯
```

A vertical stack: each label sits beside its edge.

```text
╭──────╮             
│ pane │             
╰──┬───╯             
   │ appends an event
                    
╭───────────╮        
│ event log │        
╰──┬────────╯        
   │ wakes           
                    
╭─────────╮          
│ handler │          
╰─────────╯          
```
