# Rosie Theme Schema

This document defines the supported theme file format for Rosie.

## Where theme files live

- User themes:
  - `${XDG_CONFIG_HOME:-~/.config}/rosie/themes/<name>.toml`
- Packaged themes:
  - `themes/<name>.toml` in the repository

Load resolution for `:theme <name>`:
1. user theme directory
2. packaged theme directory

The `:theme` picker (`:theme` with no args) lists user themes from `config/themes`.

## Theme name rules

- `<name>` is normalized to lowercase.
- Allowed characters: `a-z`, `0-9`, `-`, `_`.
- File extension must be `.toml`.

## Preferred schema (semantic)

Use `[ui]` + `[state]` (required), with optional `[syntax]` and `[highlight]`.

```toml
name = "my-theme"

[ui]
bg = "#191724"
panel = "#1f1d2e"
panel_alt = "#26233a"
text = "#e0def4"
text_muted = "#908caa"
border = "#403d52"
border_active = "#524f67"
title_label = "#e0def4"
title_value = "#c4a7e7"
title_value_alt = "#ebbcba"
title_meta = "#908caa"
modal_bg = "#1f1d2e"
modal_border = "#524f67"
modal_title = "#e0def4"
modal_selected_bg = "#403d52"
modal_selected_fg = "#e0def4"

[state]
accent = "#c4a7e7"
success = "#9ccfd8"
info = "#31748f"
warning = "#f6c177"
error = "#eb6f92"

[syntax]
user = "#c4a7e7"
assistant = "#e0def4"
system = "#908caa"

[highlight]
low = "#21202e"
mid = "#403d52"
high = "#524f67"
```

### Required keys

- `[ui]`:
  - `bg`, `panel`, `panel_alt`, `text`, `text_muted`, `border`, `border_active`
- `[state]`:
  - `accent`, `success`, `warning`, `error`

### Optional keys and fallbacks

- `[state].info` -> defaults to `accent`
- `[syntax].user` -> defaults to `accent`
- `[syntax].assistant` -> defaults to `text`
- `[syntax].system` -> defaults to `text_muted`
- `[highlight].low` -> defaults to `bg`
- `[highlight].mid` -> defaults to `border`
- `[highlight].high` -> defaults to `border_active`
- `[ui].title_label` -> defaults to `text`
- `[ui].title_value` -> defaults to `accent`
- `[ui].title_value_alt` -> defaults to `title_value`
- `[ui].title_meta` -> defaults to `text_muted`
- `[ui].modal_bg` -> defaults to `panel`
- `[ui].modal_border` -> defaults to `border_active`
- `[ui].modal_title` -> defaults to `title_label`
- `[ui].modal_selected_bg` -> defaults to `highlight.mid`
- `[ui].modal_selected_fg` -> defaults to `text`

## Legacy schema (compatibility)

Legacy `[colors]` files are still accepted:

```toml
[colors]
base = "#191724"
surface = "#1f1d2e"
surface_alt = "#26233a"
text = "#e0def4"
muted = "#908caa"
accent = "#c4a7e7"
success = "#9ccfd8"
warn = "#f6c177"
error = "#eb6f92"
border = "#403d52"
border_active = "#524f67"
```

For legacy files, Rosie derives missing semantic fields internally.

## Color format

- Use hex RGB: `#RRGGBB`
- `RRGGBB` without `#` is also accepted.
- Other formats are invalid.

## Applying themes

- Open picker: `:theme`
- Apply directly: `:theme <name>`

If Rosie is installed with `rosie --install`, bundled `themes/*.toml` are synced into `${XDG_CONFIG_HOME:-~/.config}/rosie/themes` so they appear in the picker.
