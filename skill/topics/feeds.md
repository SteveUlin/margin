# feeds: legacy: the older runner inside margin's daemon, and what replaces each of its triggers

`margin feed` is legacy: frozen, and it will be removed. It ran a script inside margin's daemon,
on a timer, a file watch or as a stream. Scheduling now lives outside margin, and a board is kept
current by a handler, an outside owner or a dataset. Register nothing new with it.

| the feed's trigger | what replaces it | read |
|---|---|---|
| `--every 30s` | a cron line or a systemd timer that runs `margin emit <board>.refresh`, and the script registered as the handler of that event | `margin help handlers`, `margin help integrate` |
| `--watch <path>` | `watchexec`, `entr` or a systemd path unit that runs the same `margin emit` when the file changes, and the same handler | `margin help integrate`, "A fetcher on a file change" |
| `--stream` | an outside tool that stays its own process: it follows the event stream and writes the rows it owns (`margin owner add`, then `margin apply <board> --as <owner>`), or appends values to a dataset (`margin data append`) | `margin help integrate`; `margin example runs --tool` |

A feed's script moves over as it is: it already prints what a handler prints.

```sh
margin feed rm reviews                # its last rows stay on the board, unowned
margin rm reviews pr-feed             # the feed's health line, if the board shows one
margin handler add reviews-refresh --on reviews.refresh --board reviews --owns pr -- ./fetch.sh
# crontab -e:   */5 * * * *  /path/to/margin emit reviews.refresh --project <project>
```
