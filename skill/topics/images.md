# images: a PNG in the pane

```fish
margin image plan g1 graph.png --alt "matcher pipeline" --rows 12 --parent now
```

PNG only. `--rows` is the height in terminal rows, `--alt` a short description. The doc stores the
file's absolute path, not its bytes, so leave the file where it is. The pane draws it with kitty
graphics when the terminal has them, half-blocks otherwise (config key `images`).
