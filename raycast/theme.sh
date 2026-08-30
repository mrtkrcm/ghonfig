#!/bin/bash
# @raycast.schemaVersion 1
# @raycast.title Ghostty Theme
# @raycast.mode compact
# @raycast.icon 🎨
# @raycast.argument1 { "type": "text", "placeholder": "Theme name" }
# @raycast.description Set the Ghostty theme (validated before it is written)
# @raycast.author murat

exec "$HOME/.cargo/bin/ghonfig" set theme "$1"
