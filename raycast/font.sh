#!/bin/bash
# @raycast.schemaVersion 1
# @raycast.title Ghostty Font
# @raycast.mode compact
# @raycast.icon 🔤
# @raycast.argument1 { "type": "text", "placeholder": "Font family" }
# @raycast.description Set the Ghostty font family (warns if it will not render)
# @raycast.author murat

exec "$HOME/.cargo/bin/ghonfig" set font-family "$1"
